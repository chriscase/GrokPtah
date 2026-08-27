use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use fs2::FileExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::types::{
    validate_id, validate_workspace, ComputerControlDisposition, ComputerError, ComputerErrorCode,
    ComputerResult, ComputerRun, ComputerRunState,
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

    /// Sibling durable root for isolated guest/lease records. Opening it is
    /// fail-closed and independent of semantic Computer Run recovery.
    pub fn isolated_visual_root(&self) -> PathBuf {
        self.inner.root.join("isolated-visual")
    }

    pub(crate) fn save_run(&self, run: &ComputerRun) -> ComputerResult<()> {
        let _guard = self.inner.lock.lock();
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

    pub(crate) fn claim_mutation(
        &self,
        request_id: &str,
        operation: &str,
        payload_hash: &str,
    ) -> ComputerResult<MutationClaim> {
        let _guard = self.inner.lock.lock();
        let path = self.receipt_path(request_id)?;
        if path.is_file() {
            let receipt = self.read_receipt_path(&path)?;
            if receipt.request_id != request_id
                || receipt.operation != operation
                || receipt.payload_hash != payload_hash
            {
                return Err(ComputerError::new(
                    ComputerErrorCode::Conflict,
                    "request id was reused with a different computer-use mutation",
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
        let run: ComputerRun = read_json(path).map_err(internal_error)?;
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
        if observation.target != run.target || run.state.is_terminal() {
            return Err(invalid_record());
        }
    }
    if let Some(grant) = &run.grant {
        validate_id("grant_id", &grant.grant_id)?;
        validate_id("grant_run_id", &grant.run_id)?;
        grant.target.validate()?;
        if grant.run_id != run.run_id
            || grant.target != run.target
            || grant.action_classes.is_empty()
            || grant.expires_at <= grant.issued_at
            || grant
                .uses_remaining
                .is_some_and(|remaining| remaining > run.limits.max_actions)
        {
            return Err(invalid_record());
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
            let mut run =
                ComputerRun::new(Uuid::new_v4(), None, target(), ComputerUseLimits::default())
                    .unwrap();
            run_id = run.run_id.clone();
            let now = Utc::now();
            run.grant = Some(ActionGrant {
                grant_id: "grant".into(),
                run_id: run.run_id.clone(),
                target: run.target.clone(),
                action_classes: BTreeSet::from([ActionClass::Semantic]),
                issued_by: crate::computer_use::GrantIssuer::LocalUser,
                issued_at: now,
                expires_at: now + Duration::minutes(5),
                uses_remaining: None,
                revoked_at: None,
            });
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
    fn interrupted_claim_becomes_uncertain_not_replayable() {
        let dir = tempdir().unwrap();
        let payload_hash = "a".repeat(64);
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            assert!(matches!(
                store
                    .claim_mutation("request-1", "act", &payload_hash)
                    .unwrap(),
                MutationClaim::Perform
            ));
        }
        let store = ComputerStore::open(dir.path()).unwrap();
        assert!(matches!(
            store
                .claim_mutation("request-1", "act", &payload_hash)
                .unwrap(),
            MutationClaim::Uncertain
        ));
        assert_eq!(
            store
                .claim_mutation("request-1", "act", "other")
                .unwrap_err()
                .code,
            ComputerErrorCode::Conflict
        );
    }

    #[test]
    fn corrupt_record_fails_store_open_closed() {
        let dir = tempdir().unwrap();
        {
            let store = ComputerStore::open(dir.path()).unwrap();
            let path = store.root().join("runs").join("corrupt.json");
            fs::write(path, b"{").unwrap();
        }
        assert!(ComputerStore::open(dir.path()).is_err());
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
