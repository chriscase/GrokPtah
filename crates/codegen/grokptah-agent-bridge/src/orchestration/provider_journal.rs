//! Durable provider-send journal: physical attempt identity + crash
//! reconciliation for the model helper's send machine.
//!
//! A durable run is marked `running` before any provider request exists, and
//! one logical model step can issue several *physical* sends (credential
//! refresh after 401, transport/429/5xx retry, `tool_choice` fallback,
//! non-stream fallback). Without a durable record of each physical send, a
//! crash — or a cancel — leaves no way to tell "never sent" from "may have
//! been executed by the provider", so retry and capacity release both become
//! guesses.
//!
//! This journal is deliberately *not* a second send machine. It records the
//! existing machine's physical boundary as durable state, reusing the
//! orchestration lattice's identity, hashing, error vocabulary, and
//! atomic-write discipline:
//!
//! ```text
//! KnownNotSent --> Sending --> Sent --> Responding --> Settled
//!       |             |          |          |
//!       |             +----------+----------+--> Uncertain --(reconcile)--> Settled
//!       |
//!       +--(reopen: provably never left the process)--> Settled(not_sent)
//! ```
//!
//! Invariants:
//!
//! * The `KnownNotSent` record is durable *before* the physical send. A crash
//!   observed in that state therefore proves nothing left this process.
//! * Every state after `Sending` is reached only after bytes were handed to
//!   the transport, so a crash there is `Uncertain`, never "not sent".
//! * `Uncertain` leaves only through an explicit reconciliation that proves
//!   the outcome and re-presents the exact request digest and credential
//!   revision. Reopening the store any number of times never clears it.
//! * One ordinal is one physical request. Every authorized resend allocates a
//!   new ordinal with its own request digest; an ordinal is never reused.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::store::{atomic_write_json, write_json_exclusive};
use super::types::{hash_payload, safe_id_filename, OrchError, OrchErrorCode};

/// Journal record schema. A record written by a newer schema fails closed
/// rather than being reinterpreted by an older reader.
pub const PROVIDER_JOURNAL_SCHEMA: u32 = 1;

/// Hard ceiling on physical sends recorded for one run. A send machine that
/// blows through this is refused at the durable boundary instead of growing
/// the journal without bound.
pub const MAX_PROVIDER_ATTEMPTS_PER_RUN: u64 = 512;

const MAX_LABEL_BYTES: usize = 512;
const MAX_DETAIL_BYTES: usize = 1_024;

/// Explicit durable state of one physical provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAttemptState {
    /// Intent is durable; nothing has been handed to the transport.
    KnownNotSent,
    /// The physical send is in flight.
    Sending,
    /// The provider proved receipt by producing a response head.
    Sent,
    /// Response headers are being consumed.
    Responding,
    /// The outcome is unknown and remote work may be unresolved.
    Uncertain,
    /// The outcome is durably known.
    Settled,
}

impl ProviderAttemptState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KnownNotSent => "known_not_sent",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Responding => "responding",
            Self::Uncertain => "uncertain",
            Self::Settled => "settled",
        }
    }

    /// True while the provider may still be holding unresolved work for this
    /// attempt. Retry and capacity release are both fenced on this.
    pub fn is_unresolved(self) -> bool {
        matches!(
            self,
            Self::Sending | Self::Sent | Self::Responding | Self::Uncertain
        )
    }

    /// True when a crash observed in this state proves the request never
    /// reached the transport.
    fn proves_not_sent(self) -> bool {
        matches!(self, Self::KnownNotSent)
    }
}

/// Why this physical send exists. Every value other than `InitialSend` is an
/// authorized resend and always carries a fresh ordinal and digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSendCause {
    /// First physical send for this model step.
    InitialSend,
    /// HTTP 401 with a refreshed credential revision.
    AuthRefresh,
    /// Connect-phase failure that proves the previous attempt never left.
    TransportRetry,
    /// HTTP 429.
    RateLimitRetry,
    /// HTTP 5xx / 408.
    ServerErrorRetry,
    /// HTTP 400 from a gateway that rejects the optional `tool_choice` field.
    ToolChoiceFallback,
    /// Gateway rejected or emptied the streaming contract; resend non-stream.
    StreamFallback,
}

impl ProviderSendCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InitialSend => "initial_send",
            Self::AuthRefresh => "auth_refresh",
            Self::TransportRetry => "transport_retry",
            Self::RateLimitRetry => "rate_limit_retry",
            Self::ServerErrorRetry => "server_error_retry",
            Self::ToolChoiceFallback => "tool_choice_fallback",
            Self::StreamFallback => "stream_fallback",
        }
    }
}

/// Durably known outcome of one physical send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAttemptOutcome {
    /// Proven never to have reached the provider.
    NotSent,
    /// The provider returned a complete, definitive response.
    Accepted,
    /// The provider returned a definitive non-success outcome.
    ProviderRejected,
}

impl ProviderAttemptOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotSent => "not_sent",
            Self::Accepted => "accepted",
            Self::ProviderRejected => "provider_rejected",
        }
    }
}

/// The proof an operator or reconciler presents to leave `Uncertain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderReconciliationAction {
    /// Provider-side evidence shows the request was never executed.
    ProvenNotSent,
    /// Provider-side evidence shows the request completed.
    ProvenSettled,
}

/// Everything that identifies one physical request, independent of the
/// journal record. The digest over this is what binds a durable attempt to an
/// exact run, round, route, model, credential revision, and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequestIdentity {
    /// Normalized public route template. Private compatible-gateway endpoints
    /// must arrive here already collapsed to an opaque label.
    pub route_identity: String,
    /// Provider profile id that owns the credential and the route.
    pub provider_profile: String,
    /// Wire dialect label.
    pub dialect: String,
    /// Exact wire model id.
    pub wire_model: String,
    /// Digest of the credential identity — never the credential itself. A
    /// refresh changes this, so a post-401 resend is a different attempt.
    pub credential_revision: String,
    /// Digest of the exact serialized request body.
    pub body_digest: String,
}

/// Durable record for one physical provider request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAttemptRecord {
    pub schema: u32,
    pub run_id: String,
    pub session_id: Uuid,
    /// Model-step round inside the run.
    pub round: u32,
    /// Strictly increasing physical send ordinal within the run, 1-based.
    pub ordinal: u64,
    pub cause: ProviderSendCause,
    pub route_identity: String,
    pub provider_profile: String,
    pub dialect: String,
    pub wire_model: String,
    pub credential_revision: String,
    pub body_digest: String,
    /// Binding digest over run/round/ordinal/route/profile/model/credential/body.
    pub request_digest: String,
    pub state: ProviderAttemptState,
    #[serde(default)]
    pub outcome: Option<ProviderAttemptOutcome>,
    #[serde(default)]
    pub response_status: Option<u16>,
    /// Provider-assigned request identity, when the response advertises one.
    #[serde(default)]
    pub provider_request_id: Option<String>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub uncertain_reason: Option<String>,
    #[serde(default)]
    pub reconciliation: Option<ProviderReconciliation>,
    pub declared_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The durable record of the action that resolved an uncertain attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderReconciliation {
    pub action: ProviderReconciliationAction,
    pub outcome: ProviderAttemptOutcome,
    /// Bounded operator/provider evidence. Never carries credential material.
    pub evidence: String,
    pub reconciled_at: DateTime<Utc>,
}

impl ProviderAttemptRecord {
    /// Recompute the binding digest from the record's own fields.
    pub fn request_digest_for(&self) -> String {
        hash_payload(&serde_json::json!({
            "schema": PROVIDER_JOURNAL_SCHEMA,
            "runId": self.run_id,
            "sessionId": self.session_id,
            "round": self.round,
            "ordinal": self.ordinal,
            "routeIdentity": self.route_identity,
            "providerProfile": self.provider_profile,
            "dialect": self.dialect,
            "wireModel": self.wire_model,
            "credentialRevision": self.credential_revision,
            "bodyDigest": self.body_digest,
        }))
    }

    /// Fail closed on any record this reader cannot fully account for: a
    /// foreign schema, a broken binding digest, or an inconsistent state and
    /// outcome pair.
    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema != PROVIDER_JOURNAL_SCHEMA {
            return Err(malformed("journal entry schema is unsupported"));
        }
        if self.ordinal == 0 || self.ordinal > MAX_PROVIDER_ATTEMPTS_PER_RUN {
            return Err(malformed("journal entry ordinal is out of range"));
        }
        safe_id_filename(&self.run_id).map_err(|_| malformed("journal entry run id is invalid"))?;
        for (value, field) in [
            (&self.route_identity, "route identity"),
            (&self.provider_profile, "provider profile"),
            (&self.dialect, "dialect"),
            (&self.wire_model, "wire model"),
        ] {
            if value.is_empty() || value.len() > MAX_LABEL_BYTES || value.contains('\0') {
                return Err(malformed(format!("journal entry {field} is invalid")));
            }
        }
        if !is_digest(&self.credential_revision) || !is_digest(&self.body_digest) {
            return Err(malformed("journal entry digest is invalid"));
        }
        if self.request_digest != self.request_digest_for() {
            return Err(malformed("journal entry binding digest does not match"));
        }
        match self.state {
            ProviderAttemptState::Settled if self.outcome.is_none() => {
                return Err(malformed("settled journal entry has no outcome"));
            }
            ProviderAttemptState::Settled => {}
            _ if self.outcome.is_some() => {
                return Err(malformed("unsettled journal entry carries an outcome"));
            }
            _ => {}
        }
        if self.reconciliation.is_some() && self.state != ProviderAttemptState::Settled {
            return Err(malformed("reconciled journal entry is not settled"));
        }
        Ok(())
    }

    /// True when the provider may still hold unresolved work for this record.
    pub fn is_unresolved(&self) -> bool {
        self.state.is_unresolved()
    }
}

/// One reason a run is fenced. Unreadable entries are reported too: a journal
/// this reader cannot account for is never treated as "nothing outstanding".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedAttempt {
    pub run_id: String,
    pub ordinal: Option<u64>,
    pub state: Option<ProviderAttemptState>,
    pub reason: String,
}

/// What one reopen actually changed. Reopen is idempotent: a second pass over
/// the same journal reports no new transitions and preserves every
/// `Uncertain` record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderJournalReopenReport {
    pub scanned: usize,
    /// Records durably proven never to have left this process.
    pub settled_not_sent: usize,
    /// Records that crossed the physical-send boundary before the crash.
    pub marked_uncertain: usize,
    /// Records that were already uncertain and stay that way.
    pub already_uncertain: usize,
    /// Records this reader could not account for. They keep fencing the run.
    pub unreadable: usize,
}

/// Durable journal of physical provider sends, rooted beside the durable run
/// records so both are recovered by the same store open.
#[derive(Clone)]
pub struct ProviderSendJournal {
    inner: Arc<JournalInner>,
}

struct JournalInner {
    root: PathBuf,
    lock: Mutex<()>,
}

impl ProviderSendJournal {
    /// Open the journal rooted at `root`. Cross-process exclusion is already
    /// provided by the orchestration store lock that owns this directory.
    pub fn open(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        Ok(Self {
            inner: Arc::new(JournalInner {
                root,
                lock: Mutex::new(()),
            }),
        })
    }

    fn run_dir(&self, run_id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(run_id)?;
        Ok(self.inner.root.join(safe))
    }

    fn attempt_path(&self, run_id: &str, ordinal: u64) -> Result<PathBuf, OrchError> {
        Ok(self.run_dir(run_id)?.join(format!("{ordinal:06}.json")))
    }

    /// Durably declare the intent to issue one physical request, before any
    /// byte reaches the transport. The returned record is `KnownNotSent`, so a
    /// crash between this call and [`Self::mark_sending`] is provable.
    ///
    /// The ordinal is allocated by exclusive create, so two concurrent
    /// declarations can never share one ordinal and no ordinal is reused.
    pub fn declare(
        &self,
        run_id: &str,
        session_id: Uuid,
        round: u32,
        cause: ProviderSendCause,
        identity: &ProviderRequestIdentity,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        if !is_digest(&identity.credential_revision) || !is_digest(&identity.body_digest) {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "provider attempt digests are invalid",
            ));
        }
        let _guard = self.inner.lock.lock();
        let dir = self.run_dir(run_id)?;
        fs::create_dir_all(&dir).map_err(internal)?;
        let mut ordinal = self.next_ordinal_unlocked(run_id)?;
        loop {
            if ordinal > MAX_PROVIDER_ATTEMPTS_PER_RUN {
                return Err(OrchError::new(
                    OrchErrorCode::CapacityExhausted,
                    format!(
                        "run exceeded its bound of {MAX_PROVIDER_ATTEMPTS_PER_RUN} physical provider sends"
                    ),
                ));
            }
            let now = Utc::now();
            let mut record = ProviderAttemptRecord {
                schema: PROVIDER_JOURNAL_SCHEMA,
                run_id: run_id.to_string(),
                session_id,
                round,
                ordinal,
                cause,
                route_identity: identity.route_identity.clone(),
                provider_profile: identity.provider_profile.clone(),
                dialect: identity.dialect.clone(),
                wire_model: identity.wire_model.clone(),
                credential_revision: identity.credential_revision.clone(),
                body_digest: identity.body_digest.clone(),
                request_digest: String::new(),
                state: ProviderAttemptState::KnownNotSent,
                outcome: None,
                response_status: None,
                provider_request_id: None,
                detail: None,
                uncertain_reason: None,
                reconciliation: None,
                declared_at: now,
                updated_at: now,
            };
            record.request_digest = record.request_digest_for();
            record.validate()?;
            let path = self.attempt_path(run_id, ordinal)?;
            match write_json_exclusive(&path, &record) {
                Ok(()) => return Ok(record),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    ordinal += 1;
                    continue;
                }
                Err(error) => return Err(internal(error)),
            }
        }
    }

    fn next_ordinal_unlocked(&self, run_id: &str) -> Result<u64, OrchError> {
        let dir = self.run_dir(run_id)?;
        let mut highest = 0u64;
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(1),
            Err(error) => return Err(internal(error)),
        };
        for entry in entries {
            let entry = entry.map_err(internal)?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            // An unreadable or foreign filename must never lower the next
            // ordinal: reuse is the one thing this allocation cannot do.
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                return Err(malformed("journal entry filename is not readable"));
            };
            let ordinal: u64 = stem
                .parse()
                .map_err(|_| malformed("journal entry filename is not an ordinal"))?;
            highest = highest.max(ordinal);
        }
        Ok(highest.saturating_add(1))
    }

    /// `KnownNotSent -> Sending`, durable before the physical write.
    pub fn mark_sending(
        &self,
        run_id: &str,
        ordinal: u64,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        self.transition(
            run_id,
            ordinal,
            &[ProviderAttemptState::KnownNotSent],
            |record| {
                record.state = ProviderAttemptState::Sending;
                Ok(())
            },
        )
    }

    /// `Sending -> Sent`: the provider proved receipt by responding.
    pub fn mark_sent(
        &self,
        run_id: &str,
        ordinal: u64,
        provider_request_id: Option<&str>,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        let provider_request_id = provider_request_id.map(bounded_label);
        self.transition(
            run_id,
            ordinal,
            &[ProviderAttemptState::Sending],
            move |record| {
                record.state = ProviderAttemptState::Sent;
                record.provider_request_id = provider_request_id;
                Ok(())
            },
        )
    }

    /// `Sent -> Responding`: the response head is being consumed.
    pub fn mark_responding(
        &self,
        run_id: &str,
        ordinal: u64,
        status: u16,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        self.transition(
            run_id,
            ordinal,
            &[ProviderAttemptState::Sent],
            move |record| {
                record.state = ProviderAttemptState::Responding;
                record.response_status = Some(status);
                Ok(())
            },
        )
    }

    /// Record a durably known outcome. `NotSent` is accepted only from
    /// `KnownNotSent` or `Sending`, and only when the caller can prove the
    /// request never reached the provider.
    pub fn settle(
        &self,
        run_id: &str,
        ordinal: u64,
        outcome: ProviderAttemptOutcome,
        detail: impl AsRef<str>,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        let allowed: &[ProviderAttemptState] = match outcome {
            ProviderAttemptOutcome::NotSent => &[
                ProviderAttemptState::KnownNotSent,
                ProviderAttemptState::Sending,
            ],
            ProviderAttemptOutcome::Accepted | ProviderAttemptOutcome::ProviderRejected => {
                &[ProviderAttemptState::Sent, ProviderAttemptState::Responding]
            }
        };
        let detail = bounded_detail(detail.as_ref());
        self.transition(run_id, ordinal, allowed, move |record| {
            record.state = ProviderAttemptState::Settled;
            record.outcome = Some(outcome);
            record.detail = Some(detail);
            Ok(())
        })
    }

    /// Any error after the physical-send boundary lands here. Uncertainty is
    /// sticky: it is never cleared by another failure, a reopen, or a resend.
    pub fn mark_uncertain(
        &self,
        run_id: &str,
        ordinal: u64,
        reason: impl AsRef<str>,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        let reason = bounded_detail(reason.as_ref());
        self.transition(
            run_id,
            ordinal,
            &[
                ProviderAttemptState::Sending,
                ProviderAttemptState::Sent,
                ProviderAttemptState::Responding,
                ProviderAttemptState::Uncertain,
            ],
            move |record| {
                if record.state == ProviderAttemptState::Uncertain {
                    return Ok(());
                }
                record.state = ProviderAttemptState::Uncertain;
                record.uncertain_reason = Some(reason);
                Ok(())
            },
        )
    }

    /// The only exit from `Uncertain`. The caller must re-present the exact
    /// request digest and the credential revision the attempt was issued
    /// under, so a stale or mismatched proof is refused, and an attempt that
    /// is already settled cannot be reconciled twice.
    pub fn reconcile(
        &self,
        run_id: &str,
        ordinal: u64,
        action: ProviderReconciliationAction,
        request_digest: &str,
        credential_revision: &str,
        evidence: impl AsRef<str>,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        let evidence = bounded_detail(evidence.as_ref());
        let request_digest = request_digest.to_string();
        let credential_revision = credential_revision.to_string();
        self.transition(
            run_id,
            ordinal,
            &[ProviderAttemptState::Uncertain],
            move |record| {
                if record.request_digest != request_digest {
                    return Err(OrchError::new(
                        OrchErrorCode::InvalidRequest,
                        "reconciliation does not match the recorded request digest",
                    ));
                }
                if record.credential_revision != credential_revision {
                    return Err(OrchError::new(
                        OrchErrorCode::StaleVersion,
                        "reconciliation carries a stale credential revision",
                    ));
                }
                let outcome = match action {
                    ProviderReconciliationAction::ProvenNotSent => ProviderAttemptOutcome::NotSent,
                    ProviderReconciliationAction::ProvenSettled => ProviderAttemptOutcome::Accepted,
                };
                record.state = ProviderAttemptState::Settled;
                record.outcome = Some(outcome);
                record.reconciliation = Some(ProviderReconciliation {
                    action,
                    outcome,
                    evidence: evidence.clone(),
                    reconciled_at: Utc::now(),
                });
                Ok(())
            },
        )
    }

    pub fn load(&self, run_id: &str, ordinal: u64) -> Result<ProviderAttemptRecord, OrchError> {
        let _guard = self.inner.lock.lock();
        self.load_unlocked(run_id, ordinal)
    }

    fn load_unlocked(
        &self,
        run_id: &str,
        ordinal: u64,
    ) -> Result<ProviderAttemptRecord, OrchError> {
        let path = self.attempt_path(run_id, ordinal)?;
        let text = fs::read_to_string(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                OrchError::new(OrchErrorCode::InvalidRequest, "unknown provider attempt")
            } else {
                internal(error)
            }
        })?;
        let record: ProviderAttemptRecord =
            serde_json::from_str(&text).map_err(|_| malformed("journal entry is not readable"))?;
        record.validate()?;
        if record.run_id != run_id || record.ordinal != ordinal {
            return Err(malformed("journal entry does not match its location"));
        }
        Ok(record)
    }

    fn transition<F>(
        &self,
        run_id: &str,
        ordinal: u64,
        allowed: &[ProviderAttemptState],
        apply: F,
    ) -> Result<ProviderAttemptRecord, OrchError>
    where
        F: FnOnce(&mut ProviderAttemptRecord) -> Result<(), OrchError>,
    {
        let _guard = self.inner.lock.lock();
        let mut record = self.load_unlocked(run_id, ordinal)?;
        if !allowed.contains(&record.state) {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!(
                    "provider attempt {ordinal} is {} and cannot take this transition",
                    record.state.as_str()
                ),
            ));
        }
        apply(&mut record)?;
        record.updated_at = Utc::now();
        record.validate()?;
        let path = self.attempt_path(run_id, ordinal)?;
        atomic_write_json(&path, &record).map_err(internal)?;
        Ok(record)
    }

    /// Every attempt in this run that still fences retry and capacity.
    pub fn unresolved_for_run(&self, run_id: &str) -> Result<Vec<UnresolvedAttempt>, OrchError> {
        let _guard = self.inner.lock.lock();
        let dir = self.run_dir(run_id)?;
        let mut out = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(error) => return Err(internal(error)),
        };
        for entry in entries {
            let entry = entry.map_err(internal)?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let ordinal = path
                .file_stem()
                .and_then(|value| value.to_str())
                .and_then(|stem| stem.parse::<u64>().ok());
            let record = fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<ProviderAttemptRecord>(&text).ok())
                .filter(|record| record.validate().is_ok());
            match record {
                Some(record) if record.is_unresolved() => out.push(UnresolvedAttempt {
                    run_id: run_id.to_string(),
                    ordinal: Some(record.ordinal),
                    state: Some(record.state),
                    reason: record
                        .uncertain_reason
                        .clone()
                        .unwrap_or_else(|| format!("attempt is {}", record.state.as_str())),
                }),
                Some(_) => {}
                // Fail closed: a journal entry this reader cannot account for
                // is unresolved work, not absent work.
                None => out.push(UnresolvedAttempt {
                    run_id: run_id.to_string(),
                    ordinal,
                    state: None,
                    reason: "journal entry is unreadable".into(),
                }),
            }
        }
        out.sort_by_key(|attempt| attempt.ordinal.unwrap_or(u64::MAX));
        Ok(out)
    }

    pub fn list_run(&self, run_id: &str) -> Result<Vec<ProviderAttemptRecord>, OrchError> {
        let _guard = self.inner.lock.lock();
        let dir = self.run_dir(run_id)?;
        let mut out = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(error) => return Err(internal(error)),
        };
        for entry in entries {
            let entry = entry.map_err(internal)?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path) {
                if let Ok(record) = serde_json::from_str::<ProviderAttemptRecord>(&text) {
                    if record.validate().is_ok() {
                        out.push(record);
                    }
                }
            }
        }
        out.sort_by_key(|record| record.ordinal);
        Ok(out)
    }

    /// Apply the crash cut. Records that never crossed the physical-send
    /// boundary settle as `not_sent`; everything that did becomes `Uncertain`
    /// and stays that way across any number of reopens.
    pub fn reopen(&self) -> anyhow::Result<ProviderJournalReopenReport> {
        let _guard = self.inner.lock.lock();
        let mut report = ProviderJournalReopenReport::default();
        let run_dirs = match fs::read_dir(&self.inner.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
            Err(error) => return Err(error.into()),
        };
        for run_dir in run_dirs {
            let run_dir = run_dir?.path();
            if !run_dir.is_dir() {
                continue;
            }
            for entry in fs::read_dir(&run_dir)? {
                let path = entry?.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                report.scanned += 1;
                let record = fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| serde_json::from_str::<ProviderAttemptRecord>(&text).ok())
                    .filter(|record| record.validate().is_ok());
                let Some(mut record) = record else {
                    // Left exactly as found. It keeps fencing its run.
                    report.unreadable += 1;
                    continue;
                };
                match record.state {
                    ProviderAttemptState::Uncertain => {
                        report.already_uncertain += 1;
                    }
                    ProviderAttemptState::Settled => {}
                    state if state.proves_not_sent() => {
                        record.state = ProviderAttemptState::Settled;
                        record.outcome = Some(ProviderAttemptOutcome::NotSent);
                        record.detail = Some(
                            "reopen observed a durable pre-send record; nothing left the process"
                                .into(),
                        );
                        record.updated_at = Utc::now();
                        atomic_write_json(&path, &record)?;
                        report.settled_not_sent += 1;
                    }
                    state => {
                        record.state = ProviderAttemptState::Uncertain;
                        record.uncertain_reason = Some(format!(
                            "process restarted after the physical send boundary (was {})",
                            state.as_str()
                        ));
                        record.updated_at = Utc::now();
                        atomic_write_json(&path, &record)?;
                        report.marked_uncertain += 1;
                    }
                }
            }
        }
        Ok(report)
    }
}

fn is_digest(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn bounded_label(value: &str) -> String {
    value.chars().take(MAX_LABEL_BYTES / 4).collect()
}

fn bounded_detail(value: &str) -> String {
    let clipped: String = value.chars().take(MAX_DETAIL_BYTES / 4).collect();
    if clipped.is_empty() {
        "unspecified".into()
    } else {
        clipped
    }
}

fn malformed(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::Conflict, message)
}

fn internal(error: impl std::fmt::Display) -> OrchError {
    OrchError::new(OrchErrorCode::Internal, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    fn identity(body: &str, revision: &str) -> ProviderRequestIdentity {
        ProviderRequestIdentity {
            route_identity: "https://provider.test/v1/chat/completions".into(),
            provider_profile: "test-profile".into(),
            dialect: "openai_chat_completions".into(),
            wire_model: "test-model".into(),
            credential_revision: hash_payload(&serde_json::json!({ "revision": revision })),
            body_digest: hash_payload(&serde_json::json!({ "body": body })),
        }
    }

    fn journal() -> (TempDir, ProviderSendJournal) {
        let dir = tempdir().unwrap();
        let journal = ProviderSendJournal::open(dir.path().join("provider_attempts")).unwrap();
        (dir, journal)
    }

    fn session() -> Uuid {
        Uuid::from_u128(0x5eed_0001)
    }

    /// Drive one attempt up to (but not including) the named crash cut.
    fn attempt_at(
        journal: &ProviderSendJournal,
        run_id: &str,
        cut: ProviderAttemptState,
    ) -> ProviderAttemptRecord {
        let record = journal
            .declare(
                run_id,
                session(),
                1,
                ProviderSendCause::InitialSend,
                &identity("prompt", "rev-1"),
            )
            .unwrap();
        if cut == ProviderAttemptState::KnownNotSent {
            return record;
        }
        journal.mark_sending(run_id, record.ordinal).unwrap();
        if cut == ProviderAttemptState::Sending {
            return journal.load(run_id, record.ordinal).unwrap();
        }
        journal
            .mark_sent(run_id, record.ordinal, Some("x-request-id:abc"))
            .unwrap();
        if cut == ProviderAttemptState::Sent {
            return journal.load(run_id, record.ordinal).unwrap();
        }
        journal
            .mark_responding(run_id, record.ordinal, 200)
            .unwrap();
        journal.load(run_id, record.ordinal).unwrap()
    }

    // ── crash cuts ──────────────────────────────────────────────────────

    /// The pre-send record exists precisely so a crash there is provable. It
    /// must settle as `not_sent` and must never fence retry.
    #[test]
    fn crash_before_send_is_provably_not_sent() {
        let (_dir, journal) = journal();
        let record = attempt_at(&journal, "run-a", ProviderAttemptState::KnownNotSent);
        assert_eq!(record.state, ProviderAttemptState::KnownNotSent);
        // Nothing has been handed to the transport, so this state never
        // fences retry — before or after the crash cut.
        assert!(journal.unresolved_for_run("run-a").unwrap().is_empty());

        let report = journal.reopen().unwrap();
        assert_eq!(report.settled_not_sent, 1);
        assert_eq!(report.marked_uncertain, 0);

        let recovered = journal.load("run-a", record.ordinal).unwrap();
        assert_eq!(recovered.state, ProviderAttemptState::Settled);
        assert_eq!(recovered.outcome, Some(ProviderAttemptOutcome::NotSent));
        assert!(journal.unresolved_for_run("run-a").unwrap().is_empty());
    }

    /// Every cut past the physical-send boundary is uncertain, and stays
    /// uncertain across repeated reopens.
    #[test]
    fn every_crash_cut_after_the_send_boundary_is_uncertain_and_survives_two_reopens() {
        for (index, cut) in [
            // after acceptance by the transport
            ProviderAttemptState::Sending,
            // after the response head proved receipt
            ProviderAttemptState::Sent,
            // mid-stream, and again after the stream ended but before settlement
            ProviderAttemptState::Responding,
        ]
        .into_iter()
        .enumerate()
        {
            let (_dir, journal) = journal();
            let run_id = format!("run-cut-{index}");
            let record = attempt_at(&journal, &run_id, cut);
            assert_eq!(record.state, cut);

            let first = journal.reopen().unwrap();
            assert_eq!(first.marked_uncertain, 1, "cut {cut:?}");
            assert_eq!(first.settled_not_sent, 0, "cut {cut:?}");
            let after_first = journal.load(&run_id, record.ordinal).unwrap();
            assert_eq!(after_first.state, ProviderAttemptState::Uncertain);
            let reason = after_first.uncertain_reason.clone().unwrap();
            assert!(reason.contains(cut.as_str()), "{reason}");

            // Reopen twice at every cut: uncertainty is preserved, not
            // re-derived and not cleared.
            let second = journal.reopen().unwrap();
            assert_eq!(second.marked_uncertain, 0, "cut {cut:?}");
            assert_eq!(second.already_uncertain, 1, "cut {cut:?}");
            let after_second = journal.load(&run_id, record.ordinal).unwrap();
            assert_eq!(after_second.state, ProviderAttemptState::Uncertain);
            assert_eq!(after_second.uncertain_reason, Some(reason));
            assert_eq!(journal.unresolved_for_run(&run_id).unwrap().len(), 1);
        }
    }

    /// A settled attempt is not disturbed by any number of reopens.
    #[test]
    fn reopen_never_disturbs_a_settled_attempt() {
        let (_dir, journal) = journal();
        let record = attempt_at(&journal, "run-settled", ProviderAttemptState::Responding);
        journal
            .settle(
                "run-settled",
                record.ordinal,
                ProviderAttemptOutcome::Accepted,
                "complete response",
            )
            .unwrap();
        for _ in 0..2 {
            let report = journal.reopen().unwrap();
            assert_eq!(report.marked_uncertain, 0);
            assert_eq!(report.settled_not_sent, 0);
        }
        let recovered = journal.load("run-settled", record.ordinal).unwrap();
        assert_eq!(recovered.outcome, Some(ProviderAttemptOutcome::Accepted));
        assert!(journal
            .unresolved_for_run("run-settled")
            .unwrap()
            .is_empty());
    }

    // ── identity + ordinal discipline ───────────────────────────────────

    /// Each authorized resend — refresh after 401, 429/5xx retry, tool-choice
    /// fallback, stream fallback — is a new ordinal with its own digest. No
    /// resend may silently reuse the identity of an earlier physical request.
    #[test]
    fn authorized_resends_never_reuse_an_ordinal_or_a_digest() {
        let (_dir, journal) = journal();
        let run_id = "run-resend";
        let plan = [
            (ProviderSendCause::InitialSend, "body-1", "rev-1"),
            // 401 → refreshed credential revision, same body.
            (ProviderSendCause::AuthRefresh, "body-1", "rev-2"),
            // 429 and 5xx → identical request, replayed.
            (ProviderSendCause::RateLimitRetry, "body-1", "rev-2"),
            (ProviderSendCause::ServerErrorRetry, "body-1", "rev-2"),
            // 400 → drop tool_choice, then fall back to non-stream.
            (ProviderSendCause::ToolChoiceFallback, "body-2", "rev-2"),
            (ProviderSendCause::StreamFallback, "body-3", "rev-2"),
        ];
        let mut ordinals = Vec::new();
        let mut digests = Vec::new();
        for (cause, body, revision) in plan {
            let record = journal
                .declare(run_id, session(), 4, cause, &identity(body, revision))
                .unwrap();
            assert_eq!(record.cause, cause);
            assert_eq!(record.round, 4);
            ordinals.push(record.ordinal);
            digests.push(record.request_digest);
        }
        assert_eq!(ordinals, vec![1, 2, 3, 4, 5, 6]);
        let unique: std::collections::BTreeSet<_> = digests.iter().collect();
        assert_eq!(unique.len(), digests.len(), "digests must never repeat");
    }

    /// An ordinal is one physical request: it can be sent once, and a second
    /// send against the same record is refused.
    #[test]
    fn one_physical_request_per_ordinal() {
        let (_dir, journal) = journal();
        let record = journal
            .declare(
                "run-once",
                session(),
                1,
                ProviderSendCause::InitialSend,
                &identity("prompt", "rev-1"),
            )
            .unwrap();
        journal.mark_sending("run-once", record.ordinal).unwrap();
        let second = journal
            .mark_sending("run-once", record.ordinal)
            .unwrap_err();
        assert_eq!(second.code, OrchErrorCode::Conflict);

        // Concurrent declarations allocate distinct ordinals rather than
        // colliding on one.
        let journal_a = journal.clone();
        let journal_b = journal.clone();
        let a = std::thread::spawn(move || {
            journal_a
                .declare(
                    "run-once",
                    session(),
                    1,
                    ProviderSendCause::RateLimitRetry,
                    &identity("prompt", "rev-1"),
                )
                .unwrap()
                .ordinal
        });
        let b = std::thread::spawn(move || {
            journal_b
                .declare(
                    "run-once",
                    session(),
                    1,
                    ProviderSendCause::RateLimitRetry,
                    &identity("prompt", "rev-1"),
                )
                .unwrap()
                .ordinal
        });
        let (a, b) = (a.join().unwrap(), b.join().unwrap());
        assert_ne!(a, b);
    }

    /// The binding digest must move when any of run, round, ordinal, route,
    /// profile, dialect, model, credential revision, or body changes.
    #[test]
    fn request_digest_binds_run_round_route_model_and_body() {
        let (_dir, journal) = journal();
        let base = journal
            .declare(
                "run-bind",
                session(),
                7,
                ProviderSendCause::InitialSend,
                &identity("body", "rev-1"),
            )
            .unwrap();

        let mutate = |apply: &dyn Fn(&mut ProviderAttemptRecord)| {
            let mut candidate = base.clone();
            apply(&mut candidate);
            candidate.request_digest_for()
        };
        let variants = [
            mutate(&|record| record.run_id = "run-other".into()),
            mutate(&|record| record.round = 8),
            mutate(&|record| record.ordinal = 2),
            mutate(&|record| record.session_id = Uuid::from_u128(0x5eed_0002)),
            mutate(&|record| {
                record.route_identity = "https://other.test/v1/chat/completions".into()
            }),
            mutate(&|record| record.provider_profile = "other-profile".into()),
            mutate(&|record| record.dialect = "xai_chat_completions".into()),
            mutate(&|record| record.wire_model = "other-model".into()),
            mutate(&|record| {
                record.credential_revision =
                    hash_payload(&serde_json::json!({ "revision": "rev-2" }))
            }),
            mutate(&|record| {
                record.body_digest = hash_payload(&serde_json::json!({ "body": "other" }))
            }),
        ];
        for (index, digest) in variants.iter().enumerate() {
            assert_ne!(*digest, base.request_digest, "variant {index} did not bind");
        }
        let unique: std::collections::BTreeSet<_> = variants.iter().collect();
        assert_eq!(unique.len(), variants.len());
    }

    /// The per-run ceiling is refused at the durable boundary, so a runaway
    /// send machine cannot grow the journal without bound.
    #[test]
    fn physical_send_bound_is_enforced() {
        let (_dir, journal) = journal();
        let run_dir = journal.run_dir("run-bounded").unwrap();
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(
            run_dir.join(format!("{MAX_PROVIDER_ATTEMPTS_PER_RUN:06}.json")),
            "{}",
        )
        .unwrap();
        let error = journal
            .declare(
                "run-bounded",
                session(),
                1,
                ProviderSendCause::InitialSend,
                &identity("prompt", "rev-1"),
            )
            .unwrap_err();
        assert_eq!(error.code, OrchErrorCode::CapacityExhausted);
    }

    // ── fail-closed reads ───────────────────────────────────────────────

    /// A journal entry this reader cannot account for is unresolved work, not
    /// absent work — for unparseable JSON and for a tampered binding alike.
    #[test]
    fn malformed_journal_entries_fence_the_run() {
        let (_dir, journal) = journal();
        let good = attempt_at(&journal, "run-bad", ProviderAttemptState::Responding);
        journal
            .settle(
                "run-bad",
                good.ordinal,
                ProviderAttemptOutcome::Accepted,
                "complete",
            )
            .unwrap();
        assert!(journal.unresolved_for_run("run-bad").unwrap().is_empty());

        let run_dir = journal.run_dir("run-bad").unwrap();
        fs::write(run_dir.join("000002.json"), "{ not json").unwrap();

        // A record whose fields were edited without recomputing the binding
        // digest is refused the same way.
        let mut tampered = good.clone();
        tampered.ordinal = 3;
        tampered.wire_model = "swapped-model".into();
        fs::write(
            run_dir.join("000003.json"),
            serde_json::to_vec_pretty(&tampered).unwrap(),
        )
        .unwrap();

        let unresolved = journal.unresolved_for_run("run-bad").unwrap();
        assert_eq!(unresolved.len(), 2);
        assert!(unresolved
            .iter()
            .all(|attempt| attempt.reason.contains("unreadable")));
        assert_eq!(
            journal.load("run-bad", 3).unwrap_err().code,
            OrchErrorCode::Conflict
        );

        // A reopen leaves them exactly as found, so they keep fencing.
        let report = journal.reopen().unwrap();
        assert_eq!(report.unreadable, 2);
        assert_eq!(journal.unresolved_for_run("run-bad").unwrap().len(), 2);

        // And an unreadable filename can never lower the next ordinal.
        let next = journal
            .declare(
                "run-bad",
                session(),
                1,
                ProviderSendCause::InitialSend,
                &identity("prompt", "rev-1"),
            )
            .unwrap();
        assert_eq!(next.ordinal, 4);
    }

    /// `not_sent` is a proof, not a default: it cannot be claimed once the
    /// provider has responded.
    #[test]
    fn not_sent_cannot_be_claimed_after_the_provider_responded() {
        let (_dir, journal) = journal();
        let record = attempt_at(&journal, "run-proof", ProviderAttemptState::Responding);
        let error = journal
            .settle(
                "run-proof",
                record.ordinal,
                ProviderAttemptOutcome::NotSent,
                "wishful thinking",
            )
            .unwrap_err();
        assert_eq!(error.code, OrchErrorCode::Conflict);
        assert_eq!(
            journal.load("run-proof", record.ordinal).unwrap().state,
            ProviderAttemptState::Responding
        );
    }

    // ── reconciliation ──────────────────────────────────────────────────

    /// Reconciliation must re-present the exact request digest and the
    /// credential revision the attempt was issued under, and it may resolve
    /// an attempt only once.
    #[test]
    fn reconciliation_is_exact_current_and_single_use() {
        let (_dir, journal) = journal();
        let record = attempt_at(&journal, "run-rec", ProviderAttemptState::Sent);
        journal.reopen().unwrap();
        let uncertain = journal.load("run-rec", record.ordinal).unwrap();
        assert_eq!(uncertain.state, ProviderAttemptState::Uncertain);

        let wrong_digest = journal
            .reconcile(
                "run-rec",
                record.ordinal,
                ProviderReconciliationAction::ProvenSettled,
                &hash_payload(&serde_json::json!({ "not": "this request" })),
                &uncertain.credential_revision,
                "provider dashboard shows the request completed",
            )
            .unwrap_err();
        assert_eq!(wrong_digest.code, OrchErrorCode::InvalidRequest);

        let stale_revision = journal
            .reconcile(
                "run-rec",
                record.ordinal,
                ProviderReconciliationAction::ProvenSettled,
                &uncertain.request_digest,
                &hash_payload(&serde_json::json!({ "revision": "rotated" })),
                "proof issued under a rotated credential",
            )
            .unwrap_err();
        assert_eq!(stale_revision.code, OrchErrorCode::StaleVersion);

        // Both denials leave the attempt fencing the run.
        assert_eq!(journal.unresolved_for_run("run-rec").unwrap().len(), 1);

        let settled = journal
            .reconcile(
                "run-rec",
                record.ordinal,
                ProviderReconciliationAction::ProvenNotSent,
                &uncertain.request_digest,
                &uncertain.credential_revision,
                "provider has no record of the request",
            )
            .unwrap();
        assert_eq!(settled.state, ProviderAttemptState::Settled);
        assert_eq!(settled.outcome, Some(ProviderAttemptOutcome::NotSent));
        assert!(journal.unresolved_for_run("run-rec").unwrap().is_empty());

        let duplicate = journal
            .reconcile(
                "run-rec",
                record.ordinal,
                ProviderReconciliationAction::ProvenSettled,
                &uncertain.request_digest,
                &uncertain.credential_revision,
                "second opinion",
            )
            .unwrap_err();
        assert_eq!(duplicate.code, OrchErrorCode::Conflict);

        // A reconciled attempt also survives further reopens unchanged.
        journal.reopen().unwrap();
        journal.reopen().unwrap();
        let final_state = journal.load("run-rec", record.ordinal).unwrap();
        assert_eq!(final_state.outcome, Some(ProviderAttemptOutcome::NotSent));
        assert!(final_state.reconciliation.is_some());
    }

    /// Only an uncertain attempt can be reconciled; a live or settled one
    /// cannot be talked out of its state.
    #[test]
    fn reconciliation_is_refused_outside_uncertainty() {
        let (_dir, journal) = journal();
        let record = attempt_at(&journal, "run-live", ProviderAttemptState::Sending);
        let error = journal
            .reconcile(
                "run-live",
                record.ordinal,
                ProviderReconciliationAction::ProvenSettled,
                &record.request_digest,
                &record.credential_revision,
                "premature",
            )
            .unwrap_err();
        assert_eq!(error.code, OrchErrorCode::Conflict);
    }

    /// Uncertainty is sticky: a later failure never downgrades or overwrites
    /// the reason that first fenced the attempt.
    #[test]
    fn uncertainty_is_idempotent() {
        let (_dir, journal) = journal();
        let record = attempt_at(&journal, "run-sticky", ProviderAttemptState::Sending);
        journal
            .mark_uncertain("run-sticky", record.ordinal, "connection reset mid-send")
            .unwrap();
        journal
            .mark_uncertain("run-sticky", record.ordinal, "a later, vaguer reason")
            .unwrap();
        let stored = journal.load("run-sticky", record.ordinal).unwrap();
        assert_eq!(
            stored.uncertain_reason.as_deref(),
            Some("connection reset mid-send")
        );
        assert_eq!(stored.state, ProviderAttemptState::Uncertain);
    }
}
