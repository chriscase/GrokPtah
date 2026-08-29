//! The one durable provider-attempt ledger (#478).
//!
//! ## Layout
//!
//! ```text
//! <root>/<scope-key>/<ordinal padded to 20>.json
//! ```
//!
//! One directory per send scope, one file per ordinal. Ordinals are allocated by
//! *exclusive create*: two processes racing for the same ordinal cannot both
//! win, because the loser's `create_new` fails with `AlreadyExists` and it
//! re-reads the directory. That makes the ordinal sequence monotonic across
//! processes without a shared allocator, and it makes restart reconstruction a
//! directory listing rather than a replayed log.
//!
//! ## Durability ordering
//!
//! `Preparing` is fsynced before admission is granted, and `Sending` is fsynced
//! before the send future is created. A record found at `Preparing` after a
//! crash therefore *proves* no request byte moved; a record found at `Sending`
//! or later proves nothing, and stays `Uncertain`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;

use super::crash::{checkpoint, CrashCut, CutFired};
use super::identity::{AttemptBinding, AttemptBindingSpec, OpaqueId, SendScope};
use super::record::{
    HostIncarnationId, ProviderAttempt, Settlement, SettlementContradiction, TransitionEvidence,
    PROVIDER_ATTEMPT_SCHEMA_VERSION,
};
use super::seams::{ReconciliationGrant, ReconciliationResolution};
use super::state::{HostEvidence, ProviderAttemptState, TransportEvidence};

/// Upper bound on ordinals in one scope. Reaching it fails closed rather than
/// wrapping into a reused identity.
pub const MAX_ORDINAL: u64 = 1_000_000;

/// Bounded retries when two processes contend for the same ordinal.
const MAX_ORDINAL_CONTENTION_RETRIES: u32 = 64;

#[derive(Debug)]
pub enum LedgerError {
    /// A prior attempt in this scope is not terminal, so a new ordinal cannot
    /// be admitted. This is the rule that stops `Sending`, `Uncertain`, or any
    /// in-flight attempt from silently reopening under a fresh ordinal.
    ScopeNotSettled {
        ordinal: u64,
        state: ProviderAttemptState,
    },
    /// The scope has exhausted its ordinal space.
    OrdinalExhausted,
    /// Two processes contended for an ordinal more times than is plausible.
    OrdinalContention,
    /// The requested transition is not in the lattice relation.
    IllegalTransition {
        from: ProviderAttemptState,
        to: ProviderAttemptState,
    },
    /// A compare-and-swap failed: the record moved under us.
    RevisionConflict {
        expected: u64,
        found: u64,
    },
    /// The durable record is from a schema version this build cannot interpret.
    UnknownSchema {
        found: u32,
    },
    /// The stored binding does not re-derive its own host idempotency key.
    BindingNotRederivable {
        ordinal: u64,
    },
    /// The settlement bundle contradicts itself.
    Contradiction(SettlementContradiction),
    /// Resolving an uncertain attempt requires an explicit #466 grant.
    ResolutionRequiresGrant,
    /// A crash cut fired.
    Interrupted(CutFired),
    Io(io::Error),
    Serde(serde_json::Error),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ScopeNotSettled { ordinal, state } => write!(
                f,
                "provider send scope has an unsettled attempt (ordinal {ordinal}, state {state})"
            ),
            Self::OrdinalExhausted => f.write_str("provider send scope ordinal space exhausted"),
            Self::OrdinalContention => {
                f.write_str("provider send ordinal allocation contended repeatedly")
            }
            Self::IllegalTransition { from, to } => {
                write!(f, "illegal provider attempt transition {from} -> {to}")
            }
            Self::RevisionConflict { expected, found } => write!(
                f,
                "provider attempt revision conflict (expected {expected}, found {found})"
            ),
            Self::UnknownSchema { found } => {
                write!(f, "unknown provider attempt schema version {found}")
            }
            Self::BindingNotRederivable { ordinal } => write!(
                f,
                "provider attempt {ordinal} does not re-derive its own identity"
            ),
            Self::Contradiction(inner) => write!(f, "contradictory settlement: {inner}"),
            Self::ResolutionRequiresGrant => {
                f.write_str("resolving an uncertain provider attempt requires a #466 grant")
            }
            Self::Interrupted(inner) => write!(f, "{inner}"),
            Self::Io(inner) => write!(f, "provider attempt ledger io: {inner}"),
            Self::Serde(inner) => write!(f, "provider attempt ledger encoding: {inner}"),
        }
    }
}

impl std::error::Error for LedgerError {}

impl From<io::Error> for LedgerError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for LedgerError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serde(value)
    }
}

impl From<CutFired> for LedgerError {
    fn from(value: CutFired) -> Self {
        Self::Interrupted(value)
    }
}

type Result<T> = std::result::Result<T, LedgerError>;

/// A live handle to one durable attempt. Holding it is what proves a caller is
/// bound; the physical send path takes it by reference and nothing else will do.
#[derive(Debug)]
pub struct AttemptHandle {
    record: ProviderAttempt,
    path: PathBuf,
}

impl AttemptHandle {
    pub fn binding(&self) -> &AttemptBinding {
        &self.record.binding
    }

    pub fn state(&self) -> ProviderAttemptState {
        self.record.state
    }

    pub fn ordinal(&self) -> u64 {
        self.record.ordinal()
    }

    pub fn revision(&self) -> u64 {
        self.record.revision
    }

    pub fn attempt_id(&self) -> &str {
        &self.record.attempt_id
    }

    pub fn record(&self) -> &ProviderAttempt {
        &self.record
    }

    /// The single retry rule, asked of a live attempt.
    pub fn may_auto_retry(&self) -> bool {
        self.record.may_auto_retry()
    }
}

/// What a restart found in one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Highest ordinal that exists durably, if any.
    pub max_ordinal: Option<u64>,
    /// Attempts that were `Preparing` and are now provably `NotSent`.
    pub resolved_not_sent: Vec<u64>,
    /// Attempts that were at `Sending` or later and are now `Uncertain`.
    pub left_uncertain: Vec<u64>,
    /// Attempts already terminal when recovery ran.
    pub already_terminal: Vec<u64>,
}

/// Outcome of taking over an attempt owned by another incarnation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeoverOutcome {
    /// This incarnation now owns the attempt.
    Claimed { state: ProviderAttemptState },
    /// This incarnation already owned it. Takeover is idempotent.
    AlreadyOwned { state: ProviderAttemptState },
}

/// The durable ledger.
#[derive(Debug)]
pub struct AttemptLedger {
    root: PathBuf,
    incarnation: HostIncarnationId,
}

impl AttemptLedger {
    /// Open (creating if needed) a ledger rooted at `root` for a fresh host
    /// incarnation.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        Self::open_as(root, HostIncarnationId::new_random())
    }

    /// Open a ledger under a caller-supplied incarnation identity. Recovery
    /// tests use this to play "the previous process" and "this process".
    pub fn open_as(root: impl Into<PathBuf>, incarnation: HostIncarnationId) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root, incarnation })
    }

    pub fn incarnation(&self) -> &HostIncarnationId {
        &self.incarnation
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn scope_dir(&self, scope: &SendScope) -> PathBuf {
        self.root.join(scope.ledger_key().as_str())
    }

    fn attempt_path(&self, scope: &SendScope, ordinal: u64) -> PathBuf {
        self.scope_dir(scope).join(format!("{ordinal:020}.json"))
    }

    /// Highest ordinal durably present in a scope, reconstructed by listing.
    ///
    /// This is what a restart uses: no in-memory counter survives a crash, but
    /// the directory does.
    pub fn max_ordinal(&self, scope: &SendScope) -> Result<Option<u64>> {
        let dir = self.scope_dir(scope);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut max = None;
        for entry in entries {
            let entry = entry?;
            let Some(ordinal) = Self::ordinal_of(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            max = Some(max.map_or(ordinal, |current: u64| current.max(ordinal)));
        }
        Ok(max)
    }

    fn ordinal_of(file_name: &str) -> Option<u64> {
        file_name.strip_suffix(".json")?.parse::<u64>().ok()
    }

    /// Load one attempt by scope and ordinal.
    pub fn load(&self, scope: &SendScope, ordinal: u64) -> Result<Option<ProviderAttempt>> {
        Self::read_record(&self.attempt_path(scope, ordinal))
    }

    fn read_record(path: &Path) -> Result<Option<ProviderAttempt>> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let record: ProviderAttempt = serde_json::from_slice(&bytes)?;
        if record.schema_version != PROVIDER_ATTEMPT_SCHEMA_VERSION {
            return Err(LedgerError::UnknownSchema {
                found: record.schema_version,
            });
        }
        if !record.binding.host_key_is_rederivable() {
            return Err(LedgerError::BindingNotRederivable {
                ordinal: record.ordinal(),
            });
        }
        Ok(Some(record))
    }

    /// Every attempt in a scope, ordered by ordinal.
    pub fn list_scope(&self, scope: &SendScope) -> Result<Vec<ProviderAttempt>> {
        let dir = self.scope_dir(scope);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            if Self::ordinal_of(&entry.file_name().to_string_lossy()).is_none() {
                continue;
            }
            if let Some(record) = Self::read_record(&entry.path())? {
                out.push(record);
            }
        }
        out.sort_by_key(ProviderAttempt::ordinal);
        Ok(out)
    }

    /// Persist `Preparing` and admit the attempt.
    ///
    /// Admission is refused while any earlier attempt in the same scope is
    /// non-terminal. That single rule is what makes "`Sending` or later never
    /// silently reopens with a new ordinal" true: an unresolved attempt blocks
    /// the sequence until it is resolved by evidence or by an explicit grant.
    pub fn begin_attempt(&self, spec: AttemptBindingSpec) -> Result<AttemptHandle> {
        checkpoint(CrashCut::BeforeIntent)?;
        let scope = spec.scope.clone();
        let dir = self.scope_dir(&scope);
        fs::create_dir_all(&dir)?;

        for _ in 0..MAX_ORDINAL_CONTENTION_RETRIES {
            // Re-read on every attempt: a competing process may have both
            // settled an attempt and allocated the next ordinal since last look.
            let existing = self.list_scope(&scope)?;
            if let Some(blocking) = existing.iter().find(|record| record.blocks_new_ordinal()) {
                return Err(LedgerError::ScopeNotSettled {
                    ordinal: blocking.ordinal(),
                    state: blocking.state,
                });
            }
            let next = existing
                .last()
                .map(|record| record.ordinal() + 1)
                .unwrap_or(1);
            if next > MAX_ORDINAL {
                return Err(LedgerError::OrdinalExhausted);
            }

            let binding = AttemptBinding::seal(spec.clone(), next);
            let now = Utc::now();
            let record = ProviderAttempt::new(binding, self.incarnation.clone(), now);
            let path = self.attempt_path(&scope, next);
            match write_json_exclusive(&path, &record) {
                Ok(()) => {
                    checkpoint(CrashCut::AfterPreparing)?;
                    return Ok(AttemptHandle { record, path });
                }
                // Another process took this ordinal. Look again.
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(LedgerError::OrdinalContention)
    }

    /// Persist `Sending` immediately before the send future is created.
    ///
    /// Returns only after the record is on stable storage, which is the whole
    /// point: after this call the host can no longer prove that nothing moved.
    pub fn mark_sending(&self, handle: &mut AttemptHandle) -> Result<()> {
        self.transition(
            handle,
            ProviderAttemptState::Sending,
            TransitionEvidence::PreDispatch,
        )?;
        checkpoint(CrashCut::AfterSendingBeforeBytes)?;
        Ok(())
    }

    /// Record what the transport observed. This is the only way to reach
    /// `NotSent`, `Acknowledged`, `Responding`, `Settled`, or `Uncertain` from
    /// a live send.
    pub fn apply_transport(
        &self,
        handle: &mut AttemptHandle,
        evidence: TransportEvidence,
    ) -> Result<()> {
        let next = evidence.justifies(handle.record.state);
        if next == handle.record.state {
            return Ok(());
        }
        self.transition(handle, next, TransitionEvidence::Transport(evidence))
    }

    /// Mark an attempt provably un-sent on host evidence.
    ///
    /// Legal only from `Preparing`. `Sending` can also reach `NotSent`, but
    /// only on the transport's word that the connection was never established
    /// — the host has no standing to claim non-delivery once the send future
    /// exists, so this path refuses rather than letting a caller talk its way
    /// out of uncertainty.
    pub fn mark_not_sent(&self, handle: &mut AttemptHandle, evidence: HostEvidence) -> Result<()> {
        if handle.record.state != ProviderAttemptState::Preparing {
            return Err(LedgerError::IllegalTransition {
                from: handle.record.state,
                to: ProviderAttemptState::NotSent,
            });
        }
        self.transition(
            handle,
            ProviderAttemptState::NotSent,
            TransitionEvidence::Host(evidence),
        )
    }

    fn transition(
        &self,
        handle: &mut AttemptHandle,
        to: ProviderAttemptState,
        evidence: TransitionEvidence,
    ) -> Result<()> {
        if !handle.record.state.may_transition_to(to) {
            return Err(LedgerError::IllegalTransition {
                from: handle.record.state,
                to,
            });
        }
        let mut next = handle.record.clone();
        next.push_transition(to, evidence, Utc::now());
        self.commit(handle, next)
    }

    /// Write the whole settlement bundle in one atomic rename.
    ///
    /// Settlement, cancellation, receipt, accounting, and audit outcome go to
    /// disk together or not at all, so no interruption can leave two of them
    /// disagreeing.
    pub fn settle(&self, handle: &mut AttemptHandle, settlement: Settlement) -> Result<()> {
        settlement.validate().map_err(LedgerError::Contradiction)?;
        let target = match settlement.outcome {
            super::record::SettlementOutcome::NotSent => ProviderAttemptState::NotSent,
            super::record::SettlementOutcome::Uncertain => ProviderAttemptState::Uncertain,
            super::record::SettlementOutcome::Completed
            | super::record::SettlementOutcome::ProviderRejected => ProviderAttemptState::Settled,
        };
        let mut next = handle.record.clone();
        if next.state != target {
            if !next.state.may_transition_to(target) {
                return Err(LedgerError::IllegalTransition {
                    from: next.state,
                    to: target,
                });
            }
            let evidence = match settlement.outcome {
                super::record::SettlementOutcome::NotSent => {
                    TransitionEvidence::Host(HostEvidence::OwnerObservedBeforeDispatch {
                        detail: super::state::HostFailureClass::AdmissionRefused,
                    })
                }
                _ => TransitionEvidence::Transport(TransportEvidence::ResponseComplete {
                    status: settlement.receipt.status.unwrap_or_default(),
                    bytes: settlement.accounting.response_bytes,
                }),
            };
            next.push_transition(target, evidence, settlement.settled_at);
        } else {
            next.revision = next.revision.saturating_add(1);
        }
        next.settlement = Some(settlement);
        // Both cuts sit here on purpose: the bundle is one write, so
        // "interrupted before the receipt landed" and "interrupted before the
        // audit outcome landed" are the same physical moment, and the recovery
        // assertion for both is that nothing partial is on disk.
        checkpoint(CrashCut::SettlementBeforeReceipt)?;
        checkpoint(CrashCut::SettlementBeforeAudit)?;
        self.commit(handle, next)
    }

    /// Compare-and-swap the durable record on its revision.
    fn commit(&self, handle: &mut AttemptHandle, next: ProviderAttempt) -> Result<()> {
        let current = Self::read_record(&handle.path)?;
        let found = current.as_ref().map(|record| record.revision).unwrap_or(0);
        if found != handle.record.revision {
            return Err(LedgerError::RevisionConflict {
                expected: handle.record.revision,
                found,
            });
        }
        atomic_write_json(&handle.path, &next)?;
        handle.record = next;
        Ok(())
    }

    /// Reconstruct a scope after a restart.
    ///
    /// `Preparing` records owned by a dead incarnation become `NotSent`, because
    /// `Sending` is fsynced before the send future exists. Everything from
    /// `Sending` onwards becomes `Uncertain` and stays that way: no automatic
    /// retry, no fresh ordinal, until it is resolved explicitly.
    pub fn recover_scope(&self, scope: &SendScope) -> Result<RecoveryReport> {
        let mut report = RecoveryReport {
            max_ordinal: self.max_ordinal(scope)?,
            resolved_not_sent: Vec::new(),
            left_uncertain: Vec::new(),
            already_terminal: Vec::new(),
        };
        for record in self.list_scope(scope)? {
            let ordinal = record.ordinal();
            if record.state.is_terminal() {
                report.already_terminal.push(ordinal);
                continue;
            }
            if record.state == ProviderAttemptState::Uncertain {
                report.left_uncertain.push(ordinal);
                continue;
            }
            // Our own live attempts are not orphans; only a foreign (dead)
            // incarnation's records are recovered.
            if record.owner == self.incarnation {
                continue;
            }
            let mut handle = AttemptHandle {
                path: self.attempt_path(scope, ordinal),
                record,
            };
            match handle.record.state {
                ProviderAttemptState::Preparing => {
                    let observed_incarnation = handle.record.owner.as_str().to_string();
                    self.mark_not_sent(
                        &mut handle,
                        HostEvidence::IncarnationNotLive {
                            observed_incarnation,
                        },
                    )?;
                    report.resolved_not_sent.push(ordinal);
                }
                ProviderAttemptState::Sending
                | ProviderAttemptState::Acknowledged
                | ProviderAttemptState::Responding => {
                    self.apply_transport(
                        &mut handle,
                        TransportEvidence::PossibleWriteUnresolved {
                            class: super::state::UncertaintyClass::ProcessInterrupted,
                        },
                    )?;
                    report.left_uncertain.push(ordinal);
                }
                ProviderAttemptState::NotSent
                | ProviderAttemptState::Settled
                | ProviderAttemptState::Uncertain => {}
            }
        }
        report.resolved_not_sent.sort_unstable();
        report.left_uncertain.sort_unstable();
        report.already_terminal.sort_unstable();
        Ok(report)
    }

    /// Take ownership of an attempt left by another incarnation.
    ///
    /// Revision-CAS and idempotent: taking over an attempt this incarnation
    /// already owns reports `AlreadyOwned` and writes nothing.
    pub fn takeover(&self, scope: &SendScope, ordinal: u64) -> Result<TakeoverOutcome> {
        let Some(record) = self.load(scope, ordinal)? else {
            return Err(LedgerError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "attempt not found",
            )));
        };
        if record.owner == self.incarnation {
            return Ok(TakeoverOutcome::AlreadyOwned {
                state: record.state,
            });
        }
        let state = record.state;
        let mut handle = AttemptHandle {
            path: self.attempt_path(scope, ordinal),
            record,
        };
        let mut next = handle.record.clone();
        next.owner = self.incarnation.clone();
        next.revision = next.revision.saturating_add(1);
        self.commit(&mut handle, next)?;
        Ok(TakeoverOutcome::Claimed { state })
    }

    /// Resolve an `Uncertain` attempt with an explicit #466 grant.
    ///
    /// This crate performs no provider I/O to reach a conclusion; the grant
    /// carries the conclusion, and all this does is record it consistently.
    pub fn resolve_uncertain(
        &self,
        scope: &SendScope,
        ordinal: u64,
        grant: &ReconciliationGrant,
        settlement: Settlement,
    ) -> Result<()> {
        settlement.validate().map_err(LedgerError::Contradiction)?;
        let Some(record) = self.load(scope, ordinal)? else {
            return Err(LedgerError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "attempt not found",
            )));
        };
        if record.state != ProviderAttemptState::Uncertain {
            return Err(LedgerError::IllegalTransition {
                from: record.state,
                to: ProviderAttemptState::Settled,
            });
        }
        // A grant that says "never delivered" cannot be used to claim delivery,
        // and vice versa.
        let consistent = match grant.resolution() {
            ReconciliationResolution::ObservedDelivered => matches!(
                settlement.outcome,
                super::record::SettlementOutcome::Completed
                    | super::record::SettlementOutcome::ProviderRejected
            ),
            ReconciliationResolution::ObservedNotDelivered => {
                settlement.outcome == super::record::SettlementOutcome::NotSent
            }
        };
        if !consistent {
            return Err(LedgerError::ResolutionRequiresGrant);
        }
        // The lattice forbids Uncertain -> NotSent. An out-of-band proof of
        // non-delivery is recorded as a settled attempt carrying that outcome,
        // so the durable state never claims the host proved it itself.
        let mut handle = AttemptHandle {
            path: self.attempt_path(scope, ordinal),
            record,
        };
        let mut next = handle.record.clone();
        next.push_transition(
            ProviderAttemptState::Settled,
            TransitionEvidence::ReconciliationGrant {
                grant_id: grant.grant_id().clone(),
                grant_version: grant.version(),
            },
            settlement.settled_at,
        );
        next.settlement = Some(settlement);
        self.commit(&mut handle, next)
    }

    /// Re-derive the host idempotency key a given spec and ordinal would
    /// produce. A restart uses this to confirm it is looking at *its* attempt.
    pub fn rederive_host_key(spec: &AttemptBindingSpec, ordinal: u64) -> OpaqueId {
        AttemptBinding::derive_host_key(spec, ordinal)
    }
}

fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let tmp = path.with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        use io::Write;
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Create a file that must not already exist, durably.
///
/// The hard-link install is what makes ordinal allocation safe across
/// processes: `link` fails with `EEXIST` rather than replacing a winner.
fn write_json_exclusive<T: serde::Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::other("attempt path has no filename"))?;
    let tmp = path.with_file_name(format!(".{file_name}.tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        use io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::hard_link(&tmp, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(&tmp);
    result
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
