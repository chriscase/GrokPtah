use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use fs2::FileExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::types::{
    validate_id, validate_workspace, ComputerControlDisposition, ComputerError, ComputerErrorCode,
    ComputerPrincipal, ComputerResult, ComputerRun, ComputerRunState, ComputerSurfaceBinding,
    PhysicalInputDomain, SurfaceFreshnessFence, COMPUTER_RUN_SCHEMA_VERSION,
};

const MAX_RECEIPTS: usize = 2_048;
const MAX_RECORD_BYTES: u64 = 32 * 1024 * 1024;
const TERMINAL_RUN_AGE: Duration = Duration::days(30);
const TERMINAL_RECEIPT_AGE: Duration = Duration::days(7);

#[derive(Clone)]
pub struct ComputerStore {
    inner: Arc<ComputerStoreInner>,
}

struct ComputerStoreInner {
    root: PathBuf,
    _store_lock: fs::File,
    lock: Mutex<()>,
    surfaces: Mutex<SurfaceRegistry>,
}

#[derive(Default)]
struct SurfaceRegistry {
    by_domain: HashMap<String, Arc<LiveSurfaceState>>,
    by_surface_id: HashMap<String, Arc<LiveSurfaceState>>,
}

struct LiveSurfaceState {
    binding: ComputerSurfaceBinding,
    input_domain_id: String,
    measurement_id: String,
    tick: AtomicU64,
    frame_epoch: AtomicU64,
}

/// Host-interned surface for one attested physical input domain.
#[derive(Debug, Clone)]
pub(crate) struct InternedSurface {
    pub binding: ComputerSurfaceBinding,
    pub input_domain_id: String,
    pub measurement_id: String,
}

impl InternedSurface {
    pub(crate) fn stamp_proof(
        &self,
        proof: crate::computer_use::ComputerCapabilityProof,
    ) -> ComputerResult<crate::computer_use::ComputerCapabilityProof> {
        proof.bind_to_interned_surface(&self.binding, &self.input_domain_id, &self.measurement_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReceiptState {
    Claimed,
    Succeeded,
    Failed,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MutationReceipt {
    request_id: String,
    operation: String,
    payload_hash: String,
    state: ReceiptState,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    result: Option<serde_json::Value>,
    error: Option<ComputerError>,
    #[serde(default)]
    caller_kind: Option<String>,
    #[serde(default)]
    caller_owner_session_id: Option<Uuid>,
    #[serde(default)]
    caller_agent_id: Option<String>,
    #[serde(default)]
    caller_agent_spec_revision: Option<u64>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    authority_epoch: Option<u64>,
    #[serde(default)]
    control_epoch: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct MutationStamp {
    pub principal: ComputerPrincipal,
    pub run_id: Option<String>,
    pub authority_epoch: u64,
    pub control_epoch: u64,
}

impl MutationStamp {
    pub(crate) fn from_caller(principal: ComputerPrincipal, run: Option<&ComputerRun>) -> Self {
        Self {
            principal,
            run_id: run.map(|run| run.run_id.clone()),
            authority_epoch: run.map(|run| run.authority_epoch).unwrap_or(0),
            control_epoch: run.map(|run| run.control_epoch).unwrap_or(0),
        }
    }

    fn kind(&self) -> &'static str {
        self.principal.public_kind()
    }

    fn owner_session_id(&self) -> Option<Uuid> {
        self.principal.session_id()
    }

    fn agent_id(&self) -> Option<String> {
        self.principal.agent_id().map(str::to_string)
    }

    fn agent_spec_revision(&self) -> Option<u64> {
        self.principal.agent_spec_revision()
    }
}

#[derive(Debug)]
pub(crate) enum MutationClaim {
    Perform,
    Pending,
    Uncertain,
    Replay(ComputerResult<serde_json::Value>),
}

impl ComputerStore {
    /// Durable ledger ceiling, surfaced so capacity reads report the same
    /// bound the store actually enforces in [`Self::can_create_run`].
    pub const MAX_RUN_RECORDS: usize = 256;

    pub fn open(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("runs"))?;
        fs::create_dir_all(root.join("receipts"))?;
        let root = dunce::canonicalize(root)?;
        let lock_path = root.join(".store.lock");
        let store_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        store_lock.try_lock_exclusive().map_err(|error| {
            anyhow::anyhow!(
                "computer-use store {} is already open ({error})",
                root.display()
            )
        })?;
        let store = Self {
            inner: Arc::new(ComputerStoreInner {
                root,
                _store_lock: store_lock,
                lock: Mutex::new(()),
                surfaces: Mutex::new(SurfaceRegistry::default()),
            }),
        };
        store.recover_interrupted()?;
        store.recover_receipts()?;
        store.prune_retention()?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &Path {
        &self.inner.root
    }

    pub(crate) fn save_run(&self, run: &ComputerRun) -> ComputerResult<()> {
        let _guard = self.inner.lock.lock();
        validate_run_record(run)?;
        let path = self.run_path(&run.run_id)?;
        atomic_write_json(&path, run).map_err(internal_error)
    }

    pub(crate) fn load_run(&self, run_id: &str) -> ComputerResult<Option<ComputerRun>> {
        let _guard = self.inner.lock.lock();
        self.load_run_unlocked(run_id)
    }

    pub(crate) fn update_run<F>(
        &self,
        run_id: &str,
        update: F,
    ) -> ComputerResult<Option<ComputerRun>>
    where
        F: FnOnce(&mut ComputerRun) -> ComputerResult<()>,
    {
        let _guard = self.inner.lock.lock();
        let Some(mut run) = self.load_run_unlocked(run_id)? else {
            return Ok(None);
        };
        update(&mut run)?;
        validate_run_record(&run)?;
        atomic_write_json(&self.run_path(run_id)?, &run).map_err(internal_error)?;
        Ok(Some(run))
    }

    pub(crate) fn list_runs(&self) -> ComputerResult<Vec<ComputerRun>> {
        let _guard = self.inner.lock.lock();
        self.list_runs_unlocked()
    }

    pub(crate) fn can_create_run(&self) -> ComputerResult<()> {
        let count = self.list_runs()?.len();
        if count >= Self::MAX_RUN_RECORDS {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "computer-use run record limit reached",
            ));
        }
        Ok(())
    }

    pub(crate) fn intern_physical_domain(
        &self,
        domain: &PhysicalInputDomain,
    ) -> ComputerResult<InternedSurface> {
        let mut registry = self.inner.surfaces.lock();
        if let Some(existing) = registry.by_domain.get(domain.as_key()) {
            return Ok(InternedSurface {
                binding: existing.binding.clone(),
                input_domain_id: existing.input_domain_id.clone(),
                measurement_id: existing.measurement_id.clone(),
            });
        }
        let binding = ComputerSurfaceBinding::issue();
        let state = Arc::new(LiveSurfaceState {
            binding: binding.clone(),
            input_domain_id: Uuid::new_v4().to_string(),
            measurement_id: Uuid::new_v4().to_string(),
            tick: AtomicU64::new(0),
            frame_epoch: AtomicU64::new(0),
        });
        registry
            .by_domain
            .insert(domain.as_key().to_string(), state.clone());
        registry
            .by_surface_id
            .insert(binding.surface_id.clone(), state.clone());
        Ok(InternedSurface {
            binding,
            input_domain_id: state.input_domain_id.clone(),
            measurement_id: state.measurement_id.clone(),
        })
    }

    pub(crate) fn live_freshness(
        &self,
        binding: &ComputerSurfaceBinding,
    ) -> ComputerResult<SurfaceFreshnessFence> {
        binding.validate()?;
        let registry = self.inner.surfaces.lock();
        let live = registry
            .by_surface_id
            .get(&binding.surface_id)
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::StaleObservation,
                    "surface was not interned by the host registry",
                )
            })?;
        if live.binding.incarnation != binding.incarnation {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "surface incarnation was invalidated by restart",
            ));
        }
        Ok(SurfaceFreshnessFence {
            surface_id: binding.surface_id.clone(),
            incarnation: binding.incarnation.clone(),
            tick: live.tick.load(Ordering::SeqCst),
            wall_clock: Some(Utc::now()),
        })
    }

    pub(crate) fn mint_observation_clock(
        &self,
        binding: &ComputerSurfaceBinding,
    ) -> ComputerResult<(SurfaceFreshnessFence, u64)> {
        binding.validate()?;
        let registry = self.inner.surfaces.lock();
        let live = registry
            .by_surface_id
            .get(&binding.surface_id)
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::StaleObservation,
                    "surface was not interned by the host registry",
                )
            })?;
        if live.binding.incarnation != binding.incarnation {
            return Err(ComputerError::new(
                ComputerErrorCode::StaleObservation,
                "surface incarnation was invalidated by restart",
            ));
        }
        let previous_tick = live.tick.load(Ordering::SeqCst);
        let previous_frame = live.frame_epoch.load(Ordering::SeqCst);
        let tick = live.tick.fetch_add(1, Ordering::SeqCst) + 1;
        let frame_epoch = live.frame_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        if tick <= previous_tick || frame_epoch <= previous_frame {
            return Err(ComputerError::new(
                ComputerErrorCode::Internal,
                "host surface registry clocks are not monotonic",
            ));
        }
        Ok((
            SurfaceFreshnessFence {
                surface_id: binding.surface_id.clone(),
                incarnation: binding.incarnation.clone(),
                tick,
                wall_clock: Some(Utc::now()),
            },
            frame_epoch,
        ))
    }

    pub(crate) fn claim_mutation(
        &self,
        request_id: &str,
        operation: &str,
        payload_hash: &str,
        stamp: &MutationStamp,
    ) -> ComputerResult<MutationClaim> {
        let _guard = self.inner.lock.lock();
        let path = self.receipt_path(request_id)?;
        if path.is_file() {
            let receipt = self.read_receipt_path(&path)?;
            if !receipt_is_stamped(&receipt) {
                return Ok(MutationClaim::Uncertain);
            }
            if receipt.request_id != request_id
                || receipt.operation != operation
                || receipt.payload_hash != payload_hash
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "request id was reused with a different computer-use mutation",
                ));
            }
            if !receipt_matches_stamp(&receipt, stamp) {
                return Err(ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "idempotency receipt is not bound to this caller and run authority",
                ));
            }
            return Ok(match receipt.state {
                ReceiptState::Claimed => MutationClaim::Pending,
                ReceiptState::Uncertain => MutationClaim::Uncertain,
                ReceiptState::Succeeded => {
                    MutationClaim::Replay(Ok(receipt.result.unwrap_or(serde_json::Value::Null)))
                }
                ReceiptState::Failed => {
                    MutationClaim::Replay(Err(receipt.error.unwrap_or_else(|| {
                        ComputerError::new(
                            ComputerErrorCode::Internal,
                            "stored mutation failed without an error",
                        )
                    })))
                }
            });
        }
        if count_json_files(&self.inner.root.join("receipts")).map_err(internal_error)?
            >= MAX_RECEIPTS
        {
            return Err(ComputerError::new(
                ComputerErrorCode::LimitReached,
                "computer-use mutation receipt limit reached",
            ));
        }
        let now = Utc::now();
        let receipt = MutationReceipt {
            request_id: request_id.into(),
            operation: operation.into(),
            payload_hash: payload_hash.into(),
            state: ReceiptState::Claimed,
            created_at: now,
            updated_at: now,
            result: None,
            error: None,
            caller_kind: Some(stamp.kind().to_string()),
            caller_owner_session_id: stamp.owner_session_id(),
            caller_agent_id: stamp.agent_id(),
            caller_agent_spec_revision: stamp.agent_spec_revision(),
            run_id: stamp.run_id.clone(),
            authority_epoch: Some(stamp.authority_epoch),
            control_epoch: Some(stamp.control_epoch),
        };
        write_json_exclusive(&path, &receipt).map_err(internal_error)?;
        Ok(MutationClaim::Perform)
    }

    pub(crate) fn complete_mutation(
        &self,
        request_id: &str,
        result: &ComputerResult<serde_json::Value>,
    ) -> ComputerResult<()> {
        let _guard = self.inner.lock.lock();
        let path = self.receipt_path(request_id)?;
        let mut receipt = self.read_receipt_path(&path)?;
        if receipt.state != ReceiptState::Claimed {
            return Err(ComputerError::new(
                ComputerErrorCode::Conflict,
                "computer-use mutation receipt is already terminal",
            ));
        }
        receipt.updated_at = Utc::now();
        match result {
            Ok(value) => {
                receipt.state = ReceiptState::Succeeded;
                receipt.result = Some(value.clone());
            }
            Err(error) => {
                receipt.state = ReceiptState::Failed;
                receipt.error = Some(error.clone());
            }
        }
        atomic_write_json(&path, &receipt).map_err(internal_error)
    }

    fn run_path(&self, run_id: &str) -> ComputerResult<PathBuf> {
        let safe = safe_file_id(run_id)?;
        Ok(self.inner.root.join("runs").join(format!("{safe}.json")))
    }

    fn receipt_path(&self, request_id: &str) -> ComputerResult<PathBuf> {
        let safe = safe_file_id(request_id)?;
        Ok(self
            .inner
            .root
            .join("receipts")
            .join(format!("{safe}.json")))
    }

    fn load_run_unlocked(&self, run_id: &str) -> ComputerResult<Option<ComputerRun>> {
        let path = self.run_path(run_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        self.read_run_path(&path).map(Some)
    }

    fn list_runs_unlocked(&self) -> ComputerResult<Vec<ComputerRun>> {
        let mut runs = Vec::new();
        for path in json_paths(&self.inner.root.join("runs")).map_err(internal_error)? {
            runs.push(self.read_run_path(&path)?);
        }
        runs.sort_by(|a: &ComputerRun, b: &ComputerRun| b.created_at.cmp(&a.created_at));
        Ok(runs)
    }

    fn recover_interrupted(&self) -> ComputerResult<()> {
        let _guard = self.inner.lock.lock();
        for path in json_paths(&self.inner.root.join("runs")).map_err(internal_error)? {
            let mut run = self.read_run_path(&path)?;
            if run.state.is_terminal() {
                continue;
            }
            run.state = ComputerRunState::Interrupted;
            run.set_control_disposition(ComputerControlDisposition::Interrupted);
            run.version = run.version.saturating_add(1);
            run.updated_at = Utc::now();
            run.ended_at = Some(run.updated_at);
            run.grant = None;
            run.current_observation = None;
            if run.surface.is_issued() {
                if let Ok(rotated) = run.surface.rotate_incarnation() {
                    run.surface = rotated;
                }
            }
            run.freshness_tick = 0;
            run.bump_authority_epoch();
            run.capability_proof = match &run.capability_proof {
                crate::computer_use::ComputerCapabilityProof::IndependentlyIsolatedVisualInputDomain { .. }
                | crate::computer_use::ComputerCapabilityProof::MeasuredBackgroundSafeSemantic { .. } => {
                    crate::computer_use::ComputerCapabilityProof::Unproven
                }
                other => other.clone(),
            };
            // A prior action summary can carry backend-chosen text. Clearing it
            // is the same fail-closed move as dropping the observation: restart
            // must not keep a leaky last_outcome on the durable record.
            run.last_outcome = None;
            run.last_error = Some(ComputerError::new(
                ComputerErrorCode::Interrupted,
                "computer run interrupted by process restart; explicit reauthorization required",
            ));
            // The interruption must be visible in the durable journal itself,
            // not only on the run record, so a coordinator replaying events
            // sees why the run ended (#286).
            run.record_audit(
                "recover",
                "interrupted",
                None,
                None,
                Some(ComputerErrorCode::Interrupted),
            );
            validate_run_record(&run)?;
            atomic_write_json(&path, &run).map_err(internal_error)?;
        }
        Ok(())
    }

    fn recover_receipts(&self) -> ComputerResult<()> {
        let _guard = self.inner.lock.lock();
        for path in json_paths(&self.inner.root.join("receipts")).map_err(internal_error)? {
            let mut receipt = self.read_receipt_path(&path)?;
            if receipt.state != ReceiptState::Claimed {
                continue;
            }
            receipt.state = ReceiptState::Uncertain;
            receipt.updated_at = Utc::now();
            receipt.error = Some(ComputerError::new(
                ComputerErrorCode::UncertainOutcome,
                "process stopped while the computer-use mutation was in flight; it will not be retried automatically",
            ));
            atomic_write_json(&path, &receipt).map_err(internal_error)?;
        }
        Ok(())
    }

    fn prune_retention(&self) -> ComputerResult<()> {
        let _guard = self.inner.lock.lock();
        let now = Utc::now();

        let mut runs = Vec::new();
        for path in json_paths(&self.inner.root.join("runs")).map_err(internal_error)? {
            let run = self.read_run_path(&path)?;
            runs.push((path, run));
        }
        runs.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));
        for (index, (path, run)) in runs.into_iter().enumerate() {
            let expired = run.state.is_terminal()
                && now.signed_duration_since(run.updated_at) > TERMINAL_RUN_AGE;
            if run.state.is_terminal() && (index >= Self::MAX_RUN_RECORDS || expired) {
                fs::remove_file(path).map_err(internal_error)?;
            }
        }

        let mut receipts = Vec::new();
        for path in json_paths(&self.inner.root.join("receipts")).map_err(internal_error)? {
            let receipt = self.read_receipt_path(&path)?;
            receipts.push((path, receipt));
        }
        receipts.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at));
        for (index, (path, receipt)) in receipts.into_iter().enumerate() {
            let terminal = receipt.state != ReceiptState::Claimed;
            let expired =
                terminal && now.signed_duration_since(receipt.updated_at) > TERMINAL_RECEIPT_AGE;
            if terminal && (index >= MAX_RECEIPTS || expired) {
                fs::remove_file(path).map_err(internal_error)?;
            }
        }
        Ok(())
    }

    fn read_run_path(&self, path: &Path) -> ComputerResult<ComputerRun> {
        let mut run: ComputerRun = read_json(path).map_err(internal_error)?;
        if migrate_run_record(&mut run) {
            atomic_write_json(path, &run).map_err(internal_error)?;
        }
        validate_run_record(&run)?;
        if self.run_path(&run.run_id)? != path {
            return Err(ComputerError::new(
                ComputerErrorCode::Internal,
                "computer-use run record identity does not match its durable path",
            ));
        }
        Ok(run)
    }

    fn read_receipt_path(&self, path: &Path) -> ComputerResult<MutationReceipt> {
        let receipt: MutationReceipt = read_json(path).map_err(internal_error)?;
        validate_receipt(&receipt)?;
        if self.receipt_path(&receipt.request_id)? != path {
            return Err(ComputerError::new(
                ComputerErrorCode::Internal,
                "computer-use receipt identity does not match its durable path",
            ));
        }
        Ok(receipt)
    }
}

fn migrate_run_record(run: &mut ComputerRun) -> bool {
    let mut changed = false;
    let unknown_schema =
        run.schema_version != 0 && run.schema_version != COMPUTER_RUN_SCHEMA_VERSION;
    let mut untrusted = run.schema_version == 0 || unknown_schema;

    if run
        .initiating_principal
        .as_ref()
        .is_some_and(|principal| principal.validate().is_err())
    {
        run.initiating_principal = None;
        untrusted = true;
        changed = true;
    }
    if run.initiating_principal.as_ref().is_some_and(|principal| {
        principal
            .session_id()
            .is_some_and(|session_id| session_id != run.owner_session_id)
    }) {
        run.initiating_principal = None;
        untrusted = true;
        changed = true;
    }

    let isolated_mismatch = run
        .capability_proof
        .isolated_surface()
        .is_some_and(|surface| !run.surface.is_issued() || surface != run.surface);
    let proof_invalid = run.capability_proof.validate().is_err()
        || matches!(
            run.capability_proof,
            crate::computer_use::ComputerCapabilityProof::IndependentlyIsolatedVisualInputDomain {
                origin: crate::computer_use::IsolationProofOrigin::HostNative,
                ..
            }
        )
        || isolated_mismatch;
    if proof_invalid
        || matches!(
            run.capability_proof.tier(),
            crate::computer_use::ComputerCapabilityTier::IndependentlyIsolatedVisualInputDomain
                | crate::computer_use::ComputerCapabilityTier::MeasuredBackgroundSafeSemantic
        ) && (run.schema_version == 0 || unknown_schema)
    {
        if !matches!(
            run.capability_proof,
            crate::computer_use::ComputerCapabilityProof::Unproven
        ) {
            run.capability_proof = crate::computer_use::ComputerCapabilityProof::Unproven;
            changed = true;
        }
        untrusted = true;
    }

    if let Some(grant) = &run.grant {
        let grant_untrusted = if grant.revoked_at.is_some() {
            grant.run_id != run.run_id
        } else {
            grant.validate().is_err()
                || grant.run_id != run.run_id
                || grant.target != run.target
                || grant.surface != run.surface
                || grant.authority_epoch != run.authority_epoch
                || grant.principal.as_ref() != run.initiating_principal.as_ref()
                || grant.capability_tier != run.capability_proof.tier()
                || matches!(
                    grant.capability_tier,
                    crate::computer_use::ComputerCapabilityTier::Unproven
                )
        };
        if grant_untrusted {
            run.grant = None;
            untrusted = true;
            changed = true;
        }
    }

    if let Some(observation) = &run.current_observation {
        let observation_untrusted = observation.validate(&run.limits).is_err()
            || observation.target != run.target
            || !observation.authority.surface.is_issued()
            || observation.authority.surface != run.surface
            || observation.authority.frame_epoch == 0
            || observation.authority.freshness.tick == 0
            || observation.authority.freshness.surface_id != run.surface.surface_id
            || observation.authority.freshness.incarnation != run.surface.incarnation
            || observation.authority.authority_epoch != run.authority_epoch
            || observation.authority.control_epoch != run.control_epoch
            || observation.authority.target_generation != run.target.generation;
        if observation_untrusted {
            run.current_observation = None;
            untrusted = true;
            changed = true;
        }
    }

    if untrusted {
        if run.grant.is_some() {
            run.grant = None;
            changed = true;
        }
        if run.current_observation.is_some() {
            run.current_observation = None;
            changed = true;
        }
        if run.schema_version != COMPUTER_RUN_SCHEMA_VERSION {
            run.schema_version = COMPUTER_RUN_SCHEMA_VERSION;
            changed = true;
        }
    } else if run.schema_version != COMPUTER_RUN_SCHEMA_VERSION {
        run.schema_version = COMPUTER_RUN_SCHEMA_VERSION;
        changed = true;
    }
    changed
}

fn validate_run_record(run: &ComputerRun) -> ComputerResult<()> {
    validate_id("run_id", &run.run_id)?;
    validate_workspace(run.workspace.as_deref())?;
    if let Some(parent_run_id) = &run.parent_run_id {
        validate_id("parent_run_id", parent_run_id)?;
    }
    if let Some(campaign_id) = &run.campaign_id {
        validate_id("campaign_id", campaign_id)?;
    }
    run.target.validate()?;
    run.limits.validate()?;
    if run.schema_version != COMPUTER_RUN_SCHEMA_VERSION {
        return Err(invalid_record());
    }
    if run.surface.is_issued() {
        run.surface.validate()?;
    } else if !matches!(
        run.capability_proof,
        crate::computer_use::ComputerCapabilityProof::Unproven
    ) {
        return Err(invalid_record());
    }
    run.capability_proof.validate()?;
    if matches!(
        run.capability_proof,
        crate::computer_use::ComputerCapabilityProof::IndependentlyIsolatedVisualInputDomain {
            origin: crate::computer_use::IsolationProofOrigin::HostNative,
            ..
        }
    ) {
        return Err(invalid_record());
    }
    if let Some(isolated) = run.capability_proof.isolated_surface() {
        if isolated != run.surface {
            return Err(invalid_record());
        }
    }
    if let Some(principal) = &run.initiating_principal {
        principal.validate()?;
        if principal
            .session_id()
            .is_some_and(|session_id| session_id != run.owner_session_id)
        {
            return Err(invalid_record());
        }
    }
    if run.version == 0
        || run.action_count > run.limits.max_actions
        || run.evidence_bytes > run.limits.max_evidence_bytes
        || run.audit.len() > 1_024
        || run
            .last_outcome
            .as_ref()
            .is_some_and(|outcome| outcome.summary.len() > 512)
        || run
            .last_error
            .as_ref()
            .is_some_and(|error| error.message.len() > 512)
    {
        return Err(invalid_record());
    }
    if run.state.is_terminal() != run.ended_at.is_some() {
        return Err(invalid_record());
    }
    if (run.control_disposition == ComputerControlDisposition::OperatorTakeover
        && run.state != ComputerRunState::Paused)
        || (run.control_disposition == ComputerControlDisposition::Interrupted
            && run.state != ComputerRunState::Interrupted)
        || (run.control_disposition == ComputerControlDisposition::Stopped
            && !run.state.is_terminal())
    {
        return Err(invalid_record());
    }
    if let Some(observation) = &run.current_observation {
        observation.validate(&run.limits)?;
        if observation.target != run.target
            || run.state.is_terminal()
            || observation.authority.surface != run.surface
            || observation.authority.frame_epoch == 0
            || observation.authority.freshness.tick == 0
            || observation.authority.freshness.surface_id != run.surface.surface_id
            || observation.authority.freshness.incarnation != run.surface.incarnation
            || observation.authority.authority_epoch != run.authority_epoch
            || observation.authority.control_epoch != run.control_epoch
        {
            return Err(invalid_record());
        }
    }
    if let Some(grant) = &run.grant {
        if grant.run_id != run.run_id {
            return Err(invalid_record());
        }
        if grant.revoked_at.is_some() {
            // Revoked grants are retained as bookkeeping. Revocation bumps the
            // run authority epoch, so they must not be required to match live
            // epochs or remaining uses.
        } else {
            grant.validate()?;
            if grant.target != run.target
                || grant.surface != run.surface
                || grant.authority_epoch != run.authority_epoch
                || grant.principal.as_ref() != run.initiating_principal.as_ref()
                || grant.capability_tier != run.capability_proof.tier()
                || grant
                    .uses_remaining
                    .is_some_and(|remaining| remaining > run.limits.max_actions)
            {
                return Err(invalid_record());
            }
        }
    }
    let authority_must_be_revoked =
        run.state.is_terminal() || run.state == ComputerRunState::Paused;
    if authority_must_be_revoked
        && run
            .grant
            .as_ref()
            .is_some_and(|grant| grant.revoked_at.is_none())
    {
        return Err(invalid_record());
    }
    if run.state == ComputerRunState::AwaitingAuthorization
        && (run.grant.is_some() || run.current_observation.is_some())
    {
        return Err(invalid_record());
    }
    if run
        .audit
        .windows(2)
        .any(|entries| entries[0].sequence >= entries[1].sequence)
        || run.audit.iter().any(|entry| {
            entry.operation.len() > 64
                || entry.disposition.len() > 64
                || entry
                    .observation_id
                    .as_ref()
                    .is_some_and(|id| id.len() > super::types::MAX_ID_BYTES)
        })
    {
        return Err(invalid_record());
    }
    Ok(())
}

fn receipt_is_stamped(receipt: &MutationReceipt) -> bool {
    receipt.caller_kind.is_some()
        && receipt.authority_epoch.is_some()
        && receipt.control_epoch.is_some()
}

fn receipt_matches_stamp(receipt: &MutationReceipt, stamp: &MutationStamp) -> bool {
    receipt.caller_kind.as_deref() == Some(stamp.kind())
        && receipt.caller_owner_session_id == stamp.owner_session_id()
        && receipt.caller_agent_id == stamp.agent_id()
        && receipt.caller_agent_spec_revision == stamp.agent_spec_revision()
        && receipt.run_id == stamp.run_id
        && receipt.authority_epoch == Some(stamp.authority_epoch)
        && receipt.control_epoch == Some(stamp.control_epoch)
}

fn validate_receipt(receipt: &MutationReceipt) -> ComputerResult<()> {
    validate_id("request_id", &receipt.request_id)?;
    if receipt.operation.is_empty()
        || receipt.operation.len() > 64
        || receipt.payload_hash.len() != 64
        || !receipt
            .payload_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid_record());
    }
    let payload_shape_is_valid = match receipt.state {
        ReceiptState::Claimed => receipt.result.is_none() && receipt.error.is_none(),
        ReceiptState::Succeeded => receipt.result.is_some() && receipt.error.is_none(),
        ReceiptState::Failed | ReceiptState::Uncertain => {
            receipt.result.is_none() && receipt.error.is_some()
        }
    };
    if !payload_shape_is_valid
        || receipt
            .error
            .as_ref()
            .is_some_and(|error| error.message.len() > 512)
    {
        return Err(invalid_record());
    }
    Ok(())
}

fn invalid_record() -> ComputerError {
    ComputerError::new(
        ComputerErrorCode::Internal,
        "invalid computer-use durable record",
    )
}

fn safe_file_id(id: &str) -> ComputerResult<String> {
    crate::orchestration::safe_id_filename(id).map_err(|_| {
        ComputerError::new(
            ComputerErrorCode::InvalidRequest,
            "invalid durable record id",
        )
    })
}

fn json_paths(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn count_json_files(dir: &Path) -> std::io::Result<usize> {
    Ok(json_paths(dir)?.len())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    if fs::metadata(path)?.len() > MAX_RECORD_BYTES {
        anyhow::bail!("computer-use durable record exceeds the size limit");
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    fs::File::open(path.parent().expect("record path has parent"))?.sync_all()?;
    Ok(())
}

fn write_json_exclusive<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    #[cfg(unix)]
    fs::File::open(path.parent().expect("record path has parent"))?.sync_all()?;
    Ok(())
}

fn internal_error(error: impl ToString) -> ComputerError {
    ComputerError::new(ComputerErrorCode::Internal, error.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::Duration;
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::*;
    use crate::computer_use::{
        ActionClass, ActionGrant, ActionOutcome, ComputerTarget, ComputerUseLimits,
    };

    fn target() -> ComputerTarget {
        ComputerTarget {
            app_id: "com.grokptah.demo".into(),
            window_id: "main".into(),
            generation: 1,
            display_name: "Demo".into(),
            sensitivity: crate::computer_use::Sensitivity::None,
        }
    }

    #[test]
    fn restart_interrupts_run_and_clears_authority() {
        let dir = tempdir().unwrap();
        let run_id;
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let mut run = ComputerRun::attested_foreground_for_test(
                Uuid::new_v4(),
                None,
                target(),
                ComputerUseLimits::default(),
            )
            .unwrap();
            run_id = run.run_id.clone();
            let now = Utc::now();
            run.grant = Some(ActionGrant::for_run(
                &run,
                BTreeSet::from([ActionClass::Semantic]),
                now,
                now + Duration::minutes(5),
                None,
            ));
            run.transition(ComputerRunState::Ready).unwrap();
            run.last_outcome = Some(ActionOutcome::bounded(
                "PRIVATE_DOCUMENT_TITLE leaked from AX",
                Some(true),
            ));
            store.save_run(&run).unwrap();
        }
        let store = ComputerStore::open(dir.path()).unwrap();
        let recovered = store.load_run(&run_id).unwrap().unwrap();
        assert_eq!(recovered.state, ComputerRunState::Interrupted);
        assert_eq!(
            recovered.control_disposition,
            ComputerControlDisposition::Interrupted
        );
        assert!(recovered.control_epoch > 0);
        assert!(recovered.grant.is_none());
        assert!(recovered.current_observation.is_none());
        assert!(
            recovered.last_outcome.is_none(),
            "restart must not keep a leaky last_outcome"
        );
        assert_eq!(
            recovered.last_error.as_ref().map(|error| error.code),
            Some(ComputerErrorCode::Interrupted)
        );
        let last = recovered.audit.last().expect("recovery is journaled");
        assert_eq!(last.operation, "recover");
        assert_eq!(last.disposition, "interrupted");
        assert_eq!(last.error_code, Some(ComputerErrorCode::Interrupted));
    }

    #[test]
    fn restart_rotates_incarnation_coerces_isolated_proof_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let run_id;
        let original_incarnation;
        let original_surface_id;
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let surface = crate::computer_use::ComputerSurfaceBinding::issue();
            original_incarnation = surface.incarnation.clone();
            original_surface_id = surface.surface_id.clone();
            let owner = Uuid::new_v4();
            let mut run = ComputerRun::new_with_isolation(
                owner,
                None,
                target(),
                ComputerUseLimits::default(),
                crate::computer_use::ComputerPrincipal::local_operator(owner),
                surface.clone(),
                crate::computer_use::ComputerCapabilityProof::IndependentlyIsolatedVisualInputDomain {
                    backend_id: crate::computer_use::SIMULATOR_ISOLATED_BACKEND_ID.into(),
                    surface_id: surface.surface_id.clone(),
                    incarnation: surface.incarnation.clone(),
                    input_domain_id: uuid::Uuid::new_v4().to_string(),
                    origin: crate::computer_use::IsolationProofOrigin::SimulatorFixture,
                    observe: true,
                    semantic_actions: true,
                    text_entry: true,
                    key_chords: true,
                    pointer_fallback: true,
                },
            )
            .unwrap();
            run_id = run.run_id.clone();
            let now = Utc::now();
            run.grant = Some(ActionGrant::for_run(
                &run,
                BTreeSet::from([ActionClass::Semantic]),
                now,
                now + Duration::minutes(5),
                None,
            ));
            run.transition(ComputerRunState::Ready).unwrap();
            store.save_run(&run).unwrap();
        }

        let first = ComputerStore::open(dir.path())
            .unwrap()
            .load_run(&run_id)
            .unwrap()
            .unwrap();
        assert_eq!(first.state, ComputerRunState::Interrupted);
        assert!(first.grant.is_none());
        assert_eq!(first.surface.surface_id, original_surface_id);
        assert_ne!(first.surface.incarnation, original_incarnation);
        assert_eq!(
            first.capability_proof,
            crate::computer_use::ComputerCapabilityProof::Unproven
        );
        assert_eq!(first.freshness_tick, 0);
        assert!(first.authority_epoch > 0);
        let first_incarnation = first.surface.incarnation.clone();
        let first_epoch = first.authority_epoch;
        let first_version = first.version;

        let second = ComputerStore::open(dir.path())
            .unwrap()
            .load_run(&run_id)
            .unwrap()
            .unwrap();
        assert_eq!(second.state, ComputerRunState::Interrupted);
        assert!(second.grant.is_none());
        assert_eq!(second.surface.incarnation, first_incarnation);
        assert_eq!(second.authority_epoch, first_epoch);
        assert_eq!(second.version, first_version);
        assert_eq!(
            second.capability_proof,
            crate::computer_use::ComputerCapabilityProof::Unproven
        );
    }

    fn test_stamp() -> MutationStamp {
        MutationStamp::from_caller(
            crate::computer_use::ComputerPrincipal::local_operator(Uuid::new_v4()),
            None,
        )
    }

    #[test]
    fn interrupted_claim_becomes_uncertain_not_replayable() {
        let dir = tempdir().unwrap();
        let payload_hash = "a".repeat(64);
        let stamp = test_stamp();
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            assert!(matches!(
                store
                    .claim_mutation("request-1", "act", &payload_hash, &stamp)
                    .unwrap(),
                MutationClaim::Perform
            ));
        }
        let store = ComputerStore::open(dir.path()).unwrap();
        assert!(matches!(
            store
                .claim_mutation("request-1", "act", &payload_hash, &stamp)
                .unwrap(),
            MutationClaim::Uncertain
        ));
        assert_eq!(
            store
                .claim_mutation("request-1", "act", "other", &stamp)
                .unwrap_err()
                .code,
            ComputerErrorCode::Conflict
        );
        let other = MutationStamp::from_caller(
            crate::computer_use::ComputerPrincipal::local_operator(Uuid::new_v4()),
            None,
        );
        assert_eq!(
            store
                .claim_mutation("request-1", "act", &payload_hash, &other)
                .unwrap_err()
                .code,
            ComputerErrorCode::Unauthorized
        );
    }

    #[test]
    fn unstamped_legacy_receipt_fails_closed() {
        let dir = tempdir().unwrap();
        let payload_hash = "b".repeat(64);
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let path = store.receipt_path("legacy").unwrap();
            fs::write(
                path,
                serde_json::to_vec(&serde_json::json!({
                    "requestId": "legacy",
                    "operation": "observe",
                    "payloadHash": payload_hash,
                    "state": "succeeded",
                    "createdAt": Utc::now(),
                    "updatedAt": Utc::now(),
                    "result": {"ok": true}
                }))
                .unwrap(),
            )
            .unwrap();
        }
        let store = ComputerStore::open(dir.path()).unwrap();
        assert!(matches!(
            store
                .claim_mutation("legacy", "observe", &payload_hash, &test_stamp())
                .unwrap(),
            MutationClaim::Uncertain
        ));
    }

    #[test]
    fn corrupt_json_fails_store_open_closed() {
        let dir = tempdir().unwrap();
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let path = store.root().join("runs").join("corrupt.json");
            fs::write(path, b"{").unwrap();
        }
        assert!(ComputerStore::open(dir.path()).is_err());
    }

    #[test]
    fn contradictory_legacy_records_reopen_as_interrupted_unproven() {
        let owner = Uuid::new_v4();
        let surface_a = crate::computer_use::ComputerSurfaceBinding::issue();
        let surface_b = crate::computer_use::ComputerSurfaceBinding::issue();
        let now = Utc::now();
        let cases: &[(&str, serde_json::Value)] = &[
            (
                "cross-surface-isolated",
                serde_json::json!({
                    "runId": "cross-surface-isolated",
                    "ownerSessionId": owner,
                    "target": {
                        "appId": "com.grokptah.demo",
                        "windowId": "main",
                        "generation": 1,
                        "displayName": "Demo",
                        "sensitivity": "none"
                    },
                    "state": "ready",
                    "version": 1,
                    "createdAt": now,
                    "updatedAt": now,
                    "limits": ComputerUseLimits::default(),
                    "actionCount": 0,
                    "evidenceBytes": 0,
                    "audit": [],
                    "surface": surface_a,
                    "initiatingPrincipal": {
                        "kind": "local_operator_session",
                        "sessionId": owner
                    },
                    "capabilityProof": {
                        "kind": "independently_isolated_visual_input_domain",
                        "backendId": crate::computer_use::SIMULATOR_ISOLATED_BACKEND_ID,
                        "surfaceId": surface_b.surface_id,
                        "incarnation": surface_b.incarnation,
                        "inputDomainId": Uuid::new_v4().to_string(),
                        "origin": "simulator_fixture",
                        "observe": true,
                        "semanticActions": true,
                        "textEntry": true,
                        "keyChords": true,
                        "pointerFallback": true
                    }
                }),
            ),
            (
                "agent-principal",
                serde_json::json!({
                    "runId": "agent-principal",
                    "ownerSessionId": owner,
                    "target": {
                        "appId": "com.grokptah.demo",
                        "windowId": "main",
                        "generation": 1,
                        "displayName": "Demo",
                        "sensitivity": "none"
                    },
                    "state": "ready",
                    "version": 1,
                    "createdAt": now,
                    "updatedAt": now,
                    "limits": ComputerUseLimits::default(),
                    "actionCount": 0,
                    "evidenceBytes": 0,
                    "audit": [],
                    "initiatingPrincipal": {
                        "kind": "agent",
                        "agentId": format!("agent-{}", Uuid::new_v4()),
                        "specRevision": 1
                    },
                    "capabilityProof": { "kind": "unproven" }
                }),
            ),
            (
                "missing-schema",
                serde_json::json!({
                    "runId": "missing-schema",
                    "ownerSessionId": owner,
                    "target": {
                        "appId": "com.grokptah.demo",
                        "windowId": "main",
                        "generation": 1,
                        "displayName": "Demo",
                        "sensitivity": "none"
                    },
                    "state": "ready",
                    "version": 1,
                    "createdAt": now,
                    "updatedAt": now,
                    "limits": ComputerUseLimits::default(),
                    "actionCount": 0,
                    "evidenceBytes": 0,
                    "audit": []
                }),
            ),
            (
                "unknown-schema",
                serde_json::json!({
                    "runId": "unknown-schema",
                    "ownerSessionId": owner,
                    "target": {
                        "appId": "com.grokptah.demo",
                        "windowId": "main",
                        "generation": 1,
                        "displayName": "Demo",
                        "sensitivity": "none"
                    },
                    "state": "ready",
                    "version": 1,
                    "createdAt": now,
                    "updatedAt": now,
                    "limits": ComputerUseLimits::default(),
                    "actionCount": 0,
                    "evidenceBytes": 0,
                    "audit": [],
                    "schemaVersion": 99,
                    "capabilityProof": { "kind": "unproven" }
                }),
            ),
            (
                "owner-mismatch-principal",
                serde_json::json!({
                    "runId": "owner-mismatch-principal",
                    "ownerSessionId": owner,
                    "target": {
                        "appId": "com.grokptah.demo",
                        "windowId": "main",
                        "generation": 1,
                        "displayName": "Demo",
                        "sensitivity": "none"
                    },
                    "state": "ready",
                    "version": 1,
                    "createdAt": now,
                    "updatedAt": now,
                    "limits": ComputerUseLimits::default(),
                    "actionCount": 0,
                    "evidenceBytes": 0,
                    "audit": [],
                    "schemaVersion": 1,
                    "initiatingPrincipal": {
                        "kind": "local_operator_session",
                        "sessionId": Uuid::new_v4()
                    },
                    "capabilityProof": { "kind": "unproven" }
                }),
            ),
            (
                "zero-frame-observation",
                serde_json::json!({
                    "runId": "zero-frame-observation",
                    "ownerSessionId": owner,
                    "target": {
                        "appId": "com.grokptah.demo",
                        "windowId": "main",
                        "generation": 1,
                        "displayName": "Demo",
                        "sensitivity": "none"
                    },
                    "state": "ready",
                    "version": 1,
                    "createdAt": now,
                    "updatedAt": now,
                    "limits": ComputerUseLimits::default(),
                    "actionCount": 0,
                    "evidenceBytes": 0,
                    "audit": [],
                    "schemaVersion": 1,
                    "surface": surface_a,
                    "initiatingPrincipal": {
                        "kind": "local_operator_session",
                        "sessionId": owner
                    },
                    "capabilityProof": {
                        "kind": "foreground_semantic",
                        "backendId": "test_foreground",
                        "observe": true,
                        "semanticActions": true,
                        "textEntry": true
                    },
                    "currentObservation": {
                        "observationId": "obs-1",
                        "sequence": 1,
                        "target": {
                            "appId": "com.grokptah.demo",
                            "windowId": "main",
                            "generation": 1,
                            "displayName": "Demo",
                            "sensitivity": "none"
                        },
                        "capturedAt": now,
                        "geometry": {
                            "x": 0.0,
                            "y": 0.0,
                            "width": 800.0,
                            "height": 600.0,
                            "scaleFactor": 1.0
                        },
                        "elements": [],
                        "elementsTruncated": false,
                        "sensitivity": "none",
                        "authority": {
                            "surface": surface_a,
                            "frameEpoch": 0,
                            "targetGeneration": 1,
                            "authorityEpoch": 0,
                            "controlEpoch": 0,
                            "freshness": {
                                "surfaceId": surface_a.surface_id,
                                "incarnation": surface_a.incarnation,
                                "tick": 1
                            }
                        }
                    }
                }),
            ),
        ];

        for (name, record) in cases {
            let dir = tempdir().unwrap();
            {
                let store = ComputerStore::open(dir.path()).unwrap();
                let path = store.run_path(name).unwrap();
                fs::write(path, serde_json::to_vec_pretty(record).unwrap()).unwrap();
            }
            let store = ComputerStore::open(dir.path()).unwrap();
            let recovered = store.load_run(name).unwrap().unwrap();
            assert_eq!(
                recovered.state,
                ComputerRunState::Interrupted,
                "{name} must reopen interrupted"
            );
            assert!(recovered.grant.is_none(), "{name} must drop grant");
            assert!(
                recovered.current_observation.is_none(),
                "{name} must drop observation"
            );
            if *name != "zero-frame-observation" {
                assert_eq!(
                    recovered.capability_proof,
                    crate::computer_use::ComputerCapabilityProof::Unproven,
                    "{name} must be unproven"
                );
            }
            if *name == "agent-principal" || *name == "owner-mismatch-principal" {
                assert!(recovered.initiating_principal.is_none());
            }
        }
    }

    #[test]
    fn mismatched_record_identity_fails_store_open_closed() {
        let dir = tempdir().unwrap();
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let run =
                ComputerRun::new(Uuid::new_v4(), None, target(), ComputerUseLimits::default())
                    .unwrap();
            store.save_run(&run).unwrap();
            fs::rename(
                store.run_path(&run.run_id).unwrap(),
                store.run_path("different-run").unwrap(),
            )
            .unwrap();
        }
        assert!(ComputerStore::open(dir.path()).is_err());
    }

    #[test]
    fn oversized_record_fails_before_deserialization() {
        let dir = tempdir().unwrap();
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let path = store.root().join("runs").join("oversized.json");
            let file = fs::File::create(path).unwrap();
            file.set_len(MAX_RECORD_BYTES + 1).unwrap();
        }
        assert!(ComputerStore::open(dir.path()).is_err());
    }
}
