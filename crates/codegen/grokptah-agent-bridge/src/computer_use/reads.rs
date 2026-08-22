//! Workspace-scoped Computer Run reads for the MCP control plane (#271).
//!
//! Reads need no backend: they consult only the durable store, so the control
//! plane can serve them without constructing a `ComputerUseService`. The
//! desktop GUI keeps using the session-scoped reads on the service; those
//! methods do not accept [`ComputerReadBinding`], so a coordinator surface
//! cannot be wired to them. Binding is the authorization identity: the
//! caller's claim **after** the control plane has canonicalized it and
//! matched it against the allowlist and the owning session's cwd. This layer
//! performs an exact string compare against the binding stamped on the run
//! at creation, never a filesystem lookup.
//!
//! Every run-dependent failure — unknown id, traversal-shaped id, another
//! session's run, another workspace's run, a run with no binding — collapses
//! into the identical `unauthorized` error, so none of these reads can be
//! used to probe run existence.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::projection::{
    not_available, project_events, project_run_at, ComputerRunEventPage, ComputerRunProjection,
    ComputerScopeCapacity,
};
use super::store::ComputerStore;
use super::types::{validate_id, ComputerResult, ComputerRun};

/// Authorization identity for coordinator Computer Run reads.
///
/// The workspace is the exact durable binding string after the control-plane
/// allowlist and session-cwd gate. Session-only [`super::service::ComputerUseService`]
/// methods do not accept this type, so a coordinator surface cannot be wired
/// to those methods without a type error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComputerReadBinding<'a> {
    owner_session_id: Uuid,
    workspace: &'a str,
}

impl<'a> ComputerReadBinding<'a> {
    pub fn new(owner_session_id: Uuid, workspace: &'a str) -> Self {
        Self {
            owner_session_id,
            workspace,
        }
    }

    pub fn owner_session_id(self) -> Uuid {
        self.owner_session_id
    }

    pub fn workspace(self) -> &'a str {
        self.workspace
    }
}

/// Backend-free scoped read surface over the durable Computer Run ledger.
#[derive(Clone)]
pub struct ComputerRunReads {
    store: ComputerStore,
}

impl ComputerRunReads {
    pub fn new(store: ComputerStore) -> Self {
        Self { store }
    }

    /// Projections of every run owned by one session **and** durably bound to
    /// the claimed workspace, newest first. Runs created before the binding
    /// existed carry no workspace and are invisible here by design.
    pub fn list_run_projections(
        &self,
        binding: ComputerReadBinding<'_>,
        now: DateTime<Utc>,
    ) -> ComputerResult<Vec<ComputerRunProjection>> {
        Ok(self
            .store
            .list_runs()?
            .iter()
            .filter(|run| owned_and_bound(run, binding))
            .map(|run| project_run_at(run, now))
            .collect())
    }

    /// Authoritative projection of one owned, workspace-bound run.
    pub fn project_run(
        &self,
        binding: ComputerReadBinding<'_>,
        run_id: &str,
        now: DateTime<Utc>,
    ) -> ComputerResult<ComputerRunProjection> {
        self.load_bound_run(binding, run_id)
            .map(|run| project_run_at(&run, now))
    }

    /// One bounded page of an owned, workspace-bound run's durable journal.
    pub fn run_events(
        &self,
        binding: ComputerReadBinding<'_>,
        run_id: &str,
        after_seq: Option<u64>,
        limit: usize,
    ) -> ComputerResult<ComputerRunEventPage> {
        self.load_bound_run(binding, run_id)
            .map(|run| project_events(&run, after_seq, limit))
    }

    /// Capacity scoped to the authorization identity. Host-wide occupancy is
    /// absent: after a workspace gate those figures would be a cross-scope
    /// activity oracle.
    pub fn capacity(
        &self,
        binding: ComputerReadBinding<'_>,
    ) -> ComputerResult<ComputerScopeCapacity> {
        let runs = self.store.list_runs()?;
        let scoped = runs.iter().filter(|run| owned_and_bound(run, binding));
        Ok(ComputerScopeCapacity {
            max_run_records: ComputerStore::MAX_RUN_RECORDS as u32,
            bound_runs: scoped.clone().count() as u32,
            bound_active_runs: scoped.filter(|run| !run.state.is_terminal()).count() as u32,
        })
    }

    /// Single ownership gate for every scoped read.
    fn load_bound_run(
        &self,
        binding: ComputerReadBinding<'_>,
        run_id: &str,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id).map_err(|_| not_available())?;
        self.store
            .load_run(run_id)?
            .filter(|run| owned_and_bound(run, binding))
            .ok_or_else(not_available)
    }
}

fn owned_and_bound(run: &ComputerRun, binding: ComputerReadBinding<'_>) -> bool {
    run.owner_session_id == binding.owner_session_id()
        && run.workspace.as_deref() == Some(binding.workspace())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::Duration;
    use tempfile::tempdir;

    use super::*;
    use crate::computer_use::project_run_at;
    use crate::computer_use::types::{
        ActionClass, ActionGrant, ActionOutcome, ComputerObservation, ComputerRunState,
        ComputerTarget, ComputerUseLimits, ObservationAuthority, ObservationGeometry,
        SurfaceFreshnessFence,
    };
    use crate::computer_use::Sensitivity;

    fn target() -> ComputerTarget {
        ComputerTarget {
            app_id: "com.grokptah.demo".into(),
            window_id: "main".into(),
            generation: 1,
            display_name: "Demo".into(),
            sensitivity: Sensitivity::None,
        }
    }

    fn saved_run(
        store: &ComputerStore,
        owner: Uuid,
        workspace: Option<&str>,
    ) -> crate::computer_use::ComputerRun {
        let run = ComputerRun::new(
            owner,
            workspace.map(str::to_string),
            target(),
            ComputerUseLimits::default(),
        )
        .unwrap();
        store.save_run(&run).unwrap();
        run
    }

    #[test]
    fn every_run_dependent_failure_is_the_identical_unauthorized_error() {
        let dir = tempdir().unwrap();
        let store = ComputerStore::open(dir.path()).unwrap();
        let owner = Uuid::new_v4();
        let intruder = Uuid::new_v4();
        let bound = saved_run(&store, owner, Some("/workspace/a"));
        let unbound = saved_run(&store, owner, None);
        let reads = ComputerRunReads::new(store);
        let now = Utc::now();
        let owner_a = ComputerReadBinding::new(owner, "/workspace/a");
        let owner_b = ComputerReadBinding::new(owner, "/workspace/b");
        let intruder_a = ComputerReadBinding::new(intruder, "/workspace/a");

        let baseline = reads.project_run(owner_a, "no-such-run", now).unwrap_err();
        // Cross-session, cross-workspace, unbound, and traversal-shaped reads
        // must be byte-identical to the unknown-run error.
        for error in [
            reads
                .project_run(intruder_a, &bound.run_id, now)
                .unwrap_err(),
            reads.project_run(owner_b, &bound.run_id, now).unwrap_err(),
            reads
                .project_run(owner_a, &unbound.run_id, now)
                .unwrap_err(),
            reads.project_run(owner_a, "../escape", now).unwrap_err(),
            reads
                .run_events(owner_b, &bound.run_id, None, 10)
                .unwrap_err(),
        ] {
            assert_eq!(error, baseline);
        }
        assert!(reads.project_run(owner_a, &bound.run_id, now).is_ok());
    }

    #[test]
    fn listing_and_capacity_are_scoped_to_the_workspace_binding() {
        let dir = tempdir().unwrap();
        let store = ComputerStore::open(dir.path()).unwrap();
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let mine = saved_run(&store, owner, Some("/workspace/a"));
        saved_run(&store, owner, Some("/workspace/b"));
        saved_run(&store, owner, None);
        saved_run(&store, other, Some("/workspace/a"));
        let reads = ComputerRunReads::new(store);

        let binding = ComputerReadBinding::new(owner, "/workspace/a");
        let listed = reads.list_run_projections(binding, Utc::now()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].run_id, mine.run_id);

        let capacity = reads.capacity(binding).unwrap();
        assert_eq!(capacity.bound_runs, 1);
        assert_eq!(capacity.bound_active_runs, 1);
        assert_eq!(
            capacity.max_run_records,
            ComputerStore::MAX_RUN_RECORDS as u32
        );
        let encoded = serde_json::to_value(capacity).unwrap();
        let keys: BTreeSet<&str> = encoded
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from(["maxRunRecords", "boundRuns", "boundActiveRuns"]),
            "host-wide storedRuns/activeRuns must not ride the coordinator capacity"
        );
    }

    #[test]
    fn workspace_binding_survives_restart_recovery() {
        let dir = tempdir().unwrap();
        let owner = Uuid::new_v4();
        let run_id;
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let mut run = saved_run(&store, owner, Some("/workspace/a"));
            run.last_outcome = Some(ActionOutcome::bounded(
                "PRIVATE_DOCUMENT_TITLE leaked from AX",
                Some(true),
            ));
            store.save_run(&run).unwrap();
            run_id = run.run_id;
        }
        let reads = ComputerRunReads::new(ComputerStore::open(dir.path()).unwrap());
        let binding = ComputerReadBinding::new(owner, "/workspace/a");
        let recovered = reads.project_run(binding, &run_id, Utc::now()).unwrap();
        assert_eq!(recovered.state, ComputerRunState::Interrupted);
        assert!(
            recovered.last_outcome.is_none(),
            "restart recovery must not keep a leaky last_outcome"
        );
        let encoded = serde_json::to_string(&recovered).unwrap();
        assert!(!encoded.contains("PRIVATE_DOCUMENT_TITLE"));
        assert_eq!(
            recovered.last_error.as_ref().map(|error| error.code),
            Some(crate::computer_use::ComputerErrorCode::Interrupted)
        );
        // The recovery itself is journaled and readable through the same
        // scoped read path.
        let page = reads.run_events(binding, &run_id, None, 100).unwrap();
        assert!(page
            .entries
            .iter()
            .any(|entry| entry.operation == "recover" && entry.disposition == "interrupted"));
    }

    #[test]
    fn bound_read_matches_direct_projection_including_clock_fields() {
        let dir = tempdir().unwrap();
        let store = ComputerStore::open(dir.path()).unwrap();
        let owner = Uuid::new_v4();
        let now = Utc::now();
        let mut run = ComputerRun::attested_foreground_for_test(
            owner,
            Some("/workspace/a".into()),
            target(),
            ComputerUseLimits::default(),
        )
        .unwrap();
        run.transition(ComputerRunState::Ready).unwrap();
        run.started_at = Some(now - Duration::seconds(10));
        let freshness = SurfaceFreshnessFence {
            surface_id: run.surface.surface_id.clone(),
            incarnation: run.surface.incarnation.clone(),
            tick: 1,
            wall_clock: Some(now),
        };
        run.current_observation = Some(ComputerObservation {
            observation_id: "obs-clock".into(),
            sequence: 1,
            target: run.target.clone(),
            captured_at: now - Duration::milliseconds(1),
            geometry: ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
                scale_factor: 1.0,
            },
            screenshot: None,
            elements: Vec::new(),
            elements_truncated: false,
            sensitivity: Sensitivity::None,
            authority: ObservationAuthority::bind(&run, 1, freshness).unwrap(),
        });
        run.grant = Some(ActionGrant::for_run(
            &run,
            BTreeSet::from([ActionClass::Semantic]),
            now - Duration::minutes(1),
            now + Duration::minutes(1),
            Some(2),
        ));
        run.last_outcome = Some(ActionOutcome::bounded("set demo name", Some(true)));
        store.save_run(&run).unwrap();

        let reads = ComputerRunReads::new(store);
        let binding = ComputerReadBinding::new(owner, "/workspace/a");
        let gui = project_run_at(&run, now);
        let bound = reads.project_run(binding, &run.run_id, now).unwrap();
        assert_eq!(gui, bound);
        assert_eq!(
            serde_json::to_string(&gui).unwrap(),
            serde_json::to_string(&bound).unwrap()
        );
        assert!(!gui.observation.as_ref().unwrap().stale);
        assert!(!gui.grant.as_ref().unwrap().expired);

        // Clock-derived fields move with the instant. Live MCP and GUI calls
        // that do not share `now` are not promised byte-identical.
        let later = project_run_at(&run, now + Duration::hours(1));
        assert_ne!(gui.progress.elapsed_millis, later.progress.elapsed_millis);
        assert!(later.observation.as_ref().unwrap().stale);
        assert!(later.grant.as_ref().unwrap().expired);
    }

    #[test]
    fn coordinator_dispatch_is_not_wired_to_session_only_service_methods() {
        let orch = include_str!("../orchestration/service.rs");
        let mcp = include_str!("../mcp_control.rs");
        assert!(
            orch.contains("ComputerReadBinding"),
            "coordinator reads must take ComputerReadBinding as authorization identity"
        );
        assert!(
            orch.contains("ComputerRunReads"),
            "coordinator reads must go through ComputerRunReads"
        );
        for needle in [
            "project_owned_run",
            "list_session_run_projections",
            "project_session_run",
            "session_run_events",
            "session_capacity",
            "ComputerUseService",
        ] {
            assert!(
                !orch.contains(needle),
                "orchestration must not call session-only {needle}"
            );
        }
        // mcp_control tests seed the ledger through ComputerUseService; the
        // production dispatch must still only call the orch scoped readers.
        assert!(
            mcp.contains("orch.list_computer_runs_scoped")
                && mcp.contains("orch.get_computer_run_scoped")
                && mcp.contains("orch.get_computer_run_events_scoped")
                && mcp.contains("orch.get_computer_capacity_scoped"),
            "MCP computer tools must dispatch through OrchestrationService scoped readers"
        );
        assert!(
            !mcp.contains("project_owned_run") && !mcp.contains("project_session_run"),
            "MCP dispatch must not call session-only service read methods"
        );
    }
}
