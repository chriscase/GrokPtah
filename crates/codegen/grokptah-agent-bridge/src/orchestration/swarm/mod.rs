//! Durable, host-supervised work graphs on the existing orchestration spine.
//!
//! One canonical Work/Lease/Run graph. It reuses the orchestration ledger for
//! persistence, the host's existing capacity for admission, and the Computer
//! Use ledger for grants, so there is no second scheduler, no second lease
//! universe, no second credential universe, and no authority that exists only
//! in memory.

pub mod authority;
pub mod grant;
pub mod ids;
pub mod projection;
pub mod scheduler;
pub mod spec;
pub mod state;
pub mod store;

pub use authority::{
    derive_attempt_id, derive_authority_id, ActionAuthority, AttemptState, AuthorityUse,
    PolicyRevisions, ProviderAttemptRecord, ProviderRouteSnapshot, RetryClass, SendCertainty,
};
pub use grant::{bind_grant, consume_grant_for_action, revoke_bound_grants, GrantConsumption};
pub use ids::{AttemptId, AuthorityId, GrantId, GraphId, LeaseId, WorkId, WorkerId};
pub use projection::{
    project_attribution, project_desktop, project_evidence, project_graph, project_leases,
    project_status, project_work, BoundOnlyRedactor, DesktopGraphDto, EvidenceRow, GraphProjection,
    GraphStatusProjection, LeaseRow, ProviderAttributionRow, Redactor, WorkProgressRow,
};
pub use scheduler::{
    acknowledge, cancel_graph, cancel_work, claim_spawn, forbids_same_work_retry, issue_lease,
    plan_admissions, recompute_derived, reconcile_uncertain, record_attempt_admitted,
    record_attempt_finished, recover, review_work, settle, settle_lifecycle, sweep_timeouts,
    AdmissionBlock, AdmissionPlan, DispatchIntent, DispatchProbe, RecoveryReport, ReviewDecision,
};
pub use spec::{
    FailurePolicy, GraphBudget, IsolationRequirement, QuorumGate, WorkCapability, WorkGraphSpec,
    WorkSpec, WorkerBinding, WorkerRole, WorkerSpec, WORK_GRAPH_SCHEMA_VERSION,
};
pub use state::{
    BudgetLedger, EvidenceEntry, GrantBinding, GraphLifecycle, LeaseRecord, LeaseState,
    ReviewVerdict, WorkGraphRecord, WorkOutcome, WorkRecord, WorkResult, WorkState,
};
pub use store::{ClaimOutcome, SwarmStore};
