//! Shared fixtures for the control-plane tests.
//!
//! The fixture graph is a diamond that fans out from one root, reviews both
//! branches, and synthesizes under a quorum gate:
//!
//! ```text
//!            t-root
//!            /    \
//!         t-a      t-b
//!          |        |
//!   t-review-a   t-review-b
//!            \    /
//!            t-synth   (gated on both reviews)
//! ```

#![allow(dead_code)]

use std::collections::BTreeSet;

use chrono::{DateTime, TimeZone, Utc};
use grokptah_swarm_control_plane::{
    AdmissionPolicy, BudgetPolicy, ComputerUseLeaseRef, DispatchIntent, ExternalRefId,
    FailurePolicy, IsolationRequirement, LeaseId, LeaseIssuer, ModelId, ProviderCatalog,
    ProviderCatalogEntry, ProviderId, QuorumRule, ReviewGate, SubagentCapabilityMode,
    SwarmController, SwarmId, SwarmSpec, TaskId, TaskKind, TaskOutcome, TaskSpec, WorkerCapability,
    WorkerId, WorkerRole, WorkerSpec,
};

/// A fixed clock. Every test passes explicit instants; the crate reads none.
pub fn at(offset_secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + offset_secs, 0)
        .single()
        .expect("fixed timestamp is valid")
}

pub fn provider(name: &str) -> ProviderId {
    ProviderId::parse(name).expect("valid provider id")
}

pub fn model(name: &str) -> ModelId {
    ModelId::parse(name).expect("valid model id")
}

pub fn worker_id(name: &str) -> WorkerId {
    WorkerId::parse(name).expect("valid worker id")
}

pub fn task_id(name: &str) -> TaskId {
    TaskId::parse(name).expect("valid task id")
}

fn set<T: Ord>(items: impl IntoIterator<Item = T>) -> BTreeSet<T> {
    items.into_iter().collect()
}

pub fn catalog_entry(
    provider_name: &str,
    model_name: &str,
    roles: impl IntoIterator<Item = WorkerRole>,
    capabilities: impl IntoIterator<Item = WorkerCapability>,
    modes: impl IntoIterator<Item = SubagentCapabilityMode>,
) -> ProviderCatalogEntry {
    ProviderCatalogEntry {
        provider: provider(provider_name),
        model: model(model_name),
        roles: set(roles),
        capabilities: set(capabilities),
        capability_modes: modes.into_iter().collect(),
    }
}

/// The measured provider surface the fixture draws on. Deliberately mixed: one
/// Grok implementer, one Claude reviewer/synthesizer, and one Cursor worker
/// measured for leased Computer Use.
pub fn catalog() -> ProviderCatalog {
    ProviderCatalog::new(vec![
        catalog_entry(
            "grok",
            "grok-code-fast-1",
            [WorkerRole::Implementer, WorkerRole::Explorer],
            [
                WorkerCapability::ReadWorkspace,
                WorkerCapability::WriteWorkspace,
                WorkerCapability::ExecuteInWorktree,
            ],
            [
                SubagentCapabilityMode::ReadWrite,
                SubagentCapabilityMode::ReadOnly,
            ],
        ),
        catalog_entry(
            "claude",
            "claude-opus-5",
            [WorkerRole::Reviewer, WorkerRole::Synthesizer],
            [
                WorkerCapability::ReadWorkspace,
                WorkerCapability::Review,
                WorkerCapability::Synthesize,
            ],
            [SubagentCapabilityMode::ReadOnly],
        ),
        catalog_entry(
            "cursor",
            "cursor-composer-1",
            [WorkerRole::Implementer],
            [
                WorkerCapability::ReadWorkspace,
                WorkerCapability::WriteWorkspace,
                WorkerCapability::ExecuteInWorktree,
                WorkerCapability::ComputerUseLeased,
            ],
            [SubagentCapabilityMode::ReadWrite],
        ),
    ])
}

pub fn implementer() -> WorkerSpec {
    WorkerSpec {
        worker_id: worker_id("impl-grok"),
        provider: provider("grok"),
        model: model("grok-code-fast-1"),
        role: WorkerRole::Implementer,
        capability_mode: SubagentCapabilityMode::ReadWrite,
        capabilities: set([
            WorkerCapability::ReadWorkspace,
            WorkerCapability::WriteWorkspace,
            WorkerCapability::ExecuteInWorktree,
        ]),
        isolation: IsolationRequirement::Worktree,
        credential_ref: None,
    }
}

pub fn reviewer() -> WorkerSpec {
    WorkerSpec {
        worker_id: worker_id("review-claude"),
        provider: provider("claude"),
        model: model("claude-opus-5"),
        role: WorkerRole::Reviewer,
        capability_mode: SubagentCapabilityMode::ReadOnly,
        capabilities: set([WorkerCapability::ReadWorkspace, WorkerCapability::Review]),
        isolation: IsolationRequirement::SharedReadOnly,
        credential_ref: None,
    }
}

pub fn synthesizer() -> WorkerSpec {
    WorkerSpec {
        worker_id: worker_id("synth-claude"),
        provider: provider("claude"),
        model: model("claude-opus-5"),
        role: WorkerRole::Synthesizer,
        capability_mode: SubagentCapabilityMode::ReadOnly,
        capabilities: set([
            WorkerCapability::ReadWorkspace,
            WorkerCapability::Synthesize,
        ]),
        isolation: IsolationRequirement::SharedReadOnly,
        credential_ref: None,
    }
}

/// A worker measured for leased Computer Use.
pub fn computer_use_worker() -> WorkerSpec {
    WorkerSpec {
        worker_id: worker_id("cu-cursor"),
        provider: provider("cursor"),
        model: model("cursor-composer-1"),
        role: WorkerRole::Implementer,
        capability_mode: SubagentCapabilityMode::ReadWrite,
        capabilities: set([
            WorkerCapability::ReadWorkspace,
            WorkerCapability::WriteWorkspace,
            WorkerCapability::ExecuteInWorktree,
            WorkerCapability::ComputerUseLeased,
        ]),
        isolation: IsolationRequirement::Worktree,
        credential_ref: None,
    }
}

pub fn work_task(id: &str, worker: &str, dependencies: &[&str], priority: i32) -> TaskSpec {
    TaskSpec {
        task_id: task_id(id),
        kind: TaskKind::Work,
        title: format!("work {id}"),
        instructions: format!("do the {id} portion of the objective"),
        worker_id: worker_id(worker),
        dependencies: dependencies.iter().map(|d| task_id(d)).collect(),
        priority,
        requires_computer_use: false,
        review_gate: None,
    }
}

pub fn review_task(id: &str, dependencies: &[&str]) -> TaskSpec {
    TaskSpec {
        task_id: task_id(id),
        kind: TaskKind::Review,
        title: format!("review {id}"),
        instructions: format!("review the upstream result for {id}"),
        worker_id: worker_id("review-claude"),
        dependencies: dependencies.iter().map(|d| task_id(d)).collect(),
        priority: 0,
        requires_computer_use: false,
        review_gate: None,
    }
}

pub fn synthesis_task(id: &str, reviewers: &[&str], quorum: QuorumRule) -> TaskSpec {
    TaskSpec {
        task_id: task_id(id),
        kind: TaskKind::Synthesis,
        title: format!("synthesize {id}"),
        instructions: "combine the reviewed branches".to_string(),
        worker_id: worker_id("synth-claude"),
        dependencies: reviewers.iter().map(|d| task_id(d)).collect(),
        priority: 0,
        requires_computer_use: false,
        review_gate: Some(ReviewGate {
            reviewers: reviewers.iter().map(|d| task_id(d)).collect(),
            quorum,
        }),
    }
}

/// The diamond fixture.
pub fn diamond_spec(quorum: QuorumRule) -> SwarmSpec {
    SwarmSpec {
        swarm_id: SwarmId::parse("swarm-diamond").expect("valid swarm id"),
        objective: "land the vertical slice across two branches".to_string(),
        catalog: catalog(),
        workers: vec![implementer(), reviewer(), synthesizer()],
        tasks: vec![
            work_task("t-root", "impl-grok", &[], 0),
            work_task("t-a", "impl-grok", &["t-root"], 10),
            work_task("t-b", "impl-grok", &["t-root"], 5),
            review_task("t-review-a", &["t-a"]),
            review_task("t-review-b", &["t-b"]),
            synthesis_task("t-synth", &["t-review-a", "t-review-b"], quorum),
        ],
        admission: AdmissionPolicy {
            max_in_flight: 4,
            max_fan_out: 8,
        },
        budget: BudgetPolicy::default(),
        failure: FailurePolicy::BlockDependents,
    }
}

/// A single-task swarm, for tests that only need one dispatch.
pub fn single_task_spec() -> SwarmSpec {
    SwarmSpec {
        swarm_id: SwarmId::parse("swarm-single").expect("valid swarm id"),
        objective: "one bounded step".to_string(),
        catalog: catalog(),
        workers: vec![implementer()],
        tasks: vec![work_task("t-only", "impl-grok", &[], 0)],
        admission: AdmissionPolicy {
            max_in_flight: 2,
            max_fan_out: 4,
        },
        budget: BudgetPolicy::default(),
        failure: FailurePolicy::BlockDependents,
    }
}

pub fn lease(id: &str, issued: i64, expires: i64) -> ComputerUseLeaseRef {
    ComputerUseLeaseRef {
        lease_id: LeaseId::parse(id).expect("valid lease id"),
        issued_by: LeaseIssuer::LocalOperator,
        issued_at: at(issued),
        expires_at: at(expires),
        uses_remaining: Some(1),
        revoked_at: None,
    }
}

pub fn external(id: &str) -> ExternalRefId {
    ExternalRefId::parse(id).expect("valid external ref")
}

/// Find the planned intent for one task, failing loudly if it is absent.
pub fn intent_for(intents: &[DispatchIntent], id: &str) -> DispatchIntent {
    intents
        .iter()
        .find(|intent| intent.task_id.as_str() == id)
        .unwrap_or_else(|| panic!("no dispatch intent planned for {id}"))
        .clone()
}

/// Plan, record, acknowledge, and settle one task in a single step.
pub fn run_task(swarm: &mut SwarmController, id: &str, outcome: TaskOutcome, now: DateTime<Utc>) {
    let intents = swarm.plan_dispatches(now);
    let intent = intent_for(&intents, id);
    let record = swarm
        .record_dispatch_requested(&intent, None, now)
        .expect("dispatch is admissible");
    swarm
        .record_dispatch_acknowledged(&record.dispatch_id, external(&format!("ext-{id}")), now)
        .expect("acknowledgement is legal");
    swarm
        .record_task_outcome(&record.dispatch_id, outcome, now)
        .expect("outcome is legal");
}
