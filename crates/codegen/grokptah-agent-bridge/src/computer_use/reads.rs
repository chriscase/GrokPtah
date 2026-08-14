//! Workspace-scoped Computer Run reads for the MCP control plane (#271).
//!
//! Reads need no backend: they consult only the durable store, so the control
//! plane can serve them without constructing a `ComputerUseService`. The
//! desktop GUI keeps using the session-scoped reads on the service; this
//! wrapper adds the durable workspace-binding check that MCP authorization
//! requires. `workspace` is the caller's claim **after** the control plane
//! has canonicalized it and matched it against the allowlist and the owning
//! session's cwd — this layer performs an exact string compare against the
//! binding stamped on the run at creation, never a filesystem lookup.
//!
//! Every run-dependent failure — unknown id, traversal-shaped id, another
//! session's run, another workspace's run, a run with no binding — collapses
//! into the identical `unauthorized` error, so none of these reads can be
//! used to probe run existence.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::projection::{
    not_available, project_events, project_run_at, ComputerRunCapacity, ComputerRunEventPage,
    ComputerRunProjection,
};
use super::store::ComputerStore;
use super::types::{validate_id, ComputerResult, ComputerRun};

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
        owner_session_id: Uuid,
        workspace: &str,
        now: DateTime<Utc>,
    ) -> ComputerResult<Vec<ComputerRunProjection>> {
        Ok(self
            .store
            .list_runs()?
            .iter()
            .filter(|run| owned_and_bound(run, owner_session_id, workspace))
            .map(|run| project_run_at(run, now))
            .collect())
    }

    /// Authoritative projection of one owned, workspace-bound run.
    pub fn project_run(
        &self,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        now: DateTime<Utc>,
    ) -> ComputerResult<ComputerRunProjection> {
        self.load_bound_run(owner_session_id, workspace, run_id)
            .map(|run| project_run_at(&run, now))
    }

    /// One bounded page of an owned, workspace-bound run's durable journal.
    pub fn run_events(
        &self,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        after_seq: Option<u64>,
        limit: usize,
    ) -> ComputerResult<ComputerRunEventPage> {
        self.load_bound_run(owner_session_id, workspace, run_id)
            .map(|run| project_events(&run, after_seq, limit))
    }

    /// Global ledger figures plus counts scoped to the (session, workspace)
    /// pair. Global totals are deliberate: capacity is a host-wide property
    /// under the plane's single shared bearer token, and counts reveal no run
    /// content.
    pub fn capacity(
        &self,
        owner_session_id: Uuid,
        workspace: &str,
    ) -> ComputerResult<ComputerRunCapacity> {
        let runs = self.store.list_runs()?;
        let scoped = runs
            .iter()
            .filter(|run| owned_and_bound(run, owner_session_id, workspace));
        Ok(ComputerRunCapacity {
            max_run_records: ComputerStore::MAX_RUN_RECORDS as u32,
            stored_runs: runs.len() as u32,
            active_runs: runs.iter().filter(|run| !run.state.is_terminal()).count() as u32,
            session_runs: scoped.clone().count() as u32,
            session_active_runs: scoped.filter(|run| !run.state.is_terminal()).count() as u32,
        })
    }

    /// Single ownership gate for every scoped read.
    fn load_bound_run(
        &self,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
    ) -> ComputerResult<ComputerRun> {
        validate_id("run_id", run_id).map_err(|_| not_available())?;
        self.store
            .load_run(run_id)?
            .filter(|run| owned_and_bound(run, owner_session_id, workspace))
            .ok_or_else(not_available)
    }
}

fn owned_and_bound(run: &ComputerRun, owner_session_id: Uuid, workspace: &str) -> bool {
    run.owner_session_id == owner_session_id && run.workspace.as_deref() == Some(workspace)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::computer_use::types::{ComputerRunState, ComputerTarget, ComputerUseLimits};
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

        let baseline = reads
            .project_run(owner, "/workspace/a", "no-such-run", now)
            .unwrap_err();
        // Cross-session, cross-workspace, unbound, and traversal-shaped reads
        // must be byte-identical to the unknown-run error.
        for error in [
            reads
                .project_run(intruder, "/workspace/a", &bound.run_id, now)
                .unwrap_err(),
            reads
                .project_run(owner, "/workspace/b", &bound.run_id, now)
                .unwrap_err(),
            reads
                .project_run(owner, "/workspace/a", &unbound.run_id, now)
                .unwrap_err(),
            reads
                .project_run(owner, "/workspace/a", "../escape", now)
                .unwrap_err(),
            reads
                .run_events(owner, "/workspace/b", &bound.run_id, None, 10)
                .unwrap_err(),
        ] {
            assert_eq!(error, baseline);
        }
        assert!(reads
            .project_run(owner, "/workspace/a", &bound.run_id, now)
            .is_ok());
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

        let listed = reads
            .list_run_projections(owner, "/workspace/a", Utc::now())
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].run_id, mine.run_id);

        let capacity = reads.capacity(owner, "/workspace/a").unwrap();
        assert_eq!(capacity.stored_runs, 4);
        assert_eq!(capacity.active_runs, 4);
        assert_eq!(capacity.session_runs, 1);
        assert_eq!(capacity.session_active_runs, 1);
        assert_eq!(
            capacity.max_run_records,
            ComputerStore::MAX_RUN_RECORDS as u32
        );
    }

    #[test]
    fn workspace_binding_survives_restart_recovery() {
        let dir = tempdir().unwrap();
        let owner = Uuid::new_v4();
        let run_id;
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            run_id = saved_run(&store, owner, Some("/workspace/a")).run_id;
        }
        let reads = ComputerRunReads::new(ComputerStore::open(dir.path()).unwrap());
        let recovered = reads
            .project_run(owner, "/workspace/a", &run_id, Utc::now())
            .unwrap();
        assert_eq!(recovered.state, ComputerRunState::Interrupted);
        // The recovery itself is journaled and readable through the same
        // scoped read path.
        let page = reads
            .run_events(owner, "/workspace/a", &run_id, None, 100)
            .unwrap();
        assert!(page
            .entries
            .iter()
            .any(|entry| entry.operation == "recover" && entry.disposition == "interrupted"));
    }
}
