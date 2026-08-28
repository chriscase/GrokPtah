//! Bridge-owned prompt queue and mid-turn steering primitives (#191).

use std::collections::VecDeque;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::prompt_combine::{combine_prefix_len, join_texts, CombineGate};
use crate::queue_authority::{DeliveryGate, QueueActor, QueueOwnerKey, QueueProvenance};

const MAX_PROMPT_BYTES: usize = 100_000;
const LARGE_STEERING_BYTES: usize = 25_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptQueueEntry {
    pub id: String,
    pub version: u64,
    pub text: String,
    pub kind: String,
    pub source: String,
    /// Opaque wire principal (`desktop`, `mcp`, or a named credential id).
    ///
    /// Unchanged in shape and meaning for existing consumers. It is a *label*,
    /// not an authority: two principals could historically share `mcp`, which
    /// is precisely why `owner_key` below exists.
    pub owner: Option<String>,
    /// Canonical ownership handle (#461).
    ///
    /// `None` means the entry predates principal ownership and is quarantined:
    /// no principal owns it, so no principal may read, mutate, or run it. It is
    /// retained rather than deleted so audit evidence survives the migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_key: Option<String>,
    /// Authentication epoch and policy revision this entry was stamped under.
    ///
    /// Provenance only: recorded and audited, never consulted to grant access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_provenance: Option<QueueProvenance>,
    pub created_at: DateTime<Utc>,
    pub priority: bool,
}

impl PromptQueueEntry {
    /// Create an entry owned by `actor`.
    ///
    /// There is no constructor that leaves ownership unset: an entry that
    /// cannot name its owner cannot be created, only loaded from a legacy
    /// persisted queue.
    pub fn new(
        text: impl Into<String>,
        source: impl Into<String>,
        priority: bool,
        actor: &QueueActor,
    ) -> Result<Self> {
        let text = clean_prompt(text.into())?;
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            version: 0,
            kind: classify_kind(&text).into(),
            text,
            source: source.into(),
            owner: Some(actor.origin().to_string()),
            owner_key: Some(actor.key().as_str().to_string()),
            owner_provenance: Some(actor.provenance()),
            created_at: Utc::now(),
            priority,
        })
    }

    /// Whether `key` owns this entry.
    ///
    /// A quarantined entry (`owner_key == None`) is owned by nobody, so this is
    /// false for every caller — including the one that originally queued it.
    pub fn owned_by(&self, key: &QueueOwnerKey) -> bool {
        self.owner_key.as_deref() == Some(key.as_str())
    }

    /// Whether this entry predates principal ownership.
    pub fn is_quarantined(&self) -> bool {
        self.owner_key.is_none()
    }
}

/// The single refusal used for unknown, malformed, foreign, and quarantined
/// queue ids.
///
/// All four must be byte-identical or the queue becomes an existence oracle: a
/// principal could enumerate ids and learn which ones exist, and which belong
/// to someone else, from the shape of the refusal. The id is deliberately not
/// interpolated — echoing it back is both a needless disclosure and a way for
/// the message to differ between cases.
///
/// Callers must reach this only through a scoped lookup, so that "no such
/// entry" and "not yours" are the *same code path* rather than two paths that
/// happen to format the same string today.
pub(crate) fn unknown_queued_prompt() -> anyhow::Error {
    anyhow::anyhow!("unknown queued prompt")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptQueueBatch {
    pub entries: Vec<PromptQueueEntry>,
    pub text: String,
}

/// A queue read together with the revision it was taken at.
///
/// Consumers apply this through the same watermark as `PromptQueueChanged`,
/// so a slow read cannot overwrite a newer event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptQueueSnapshot {
    pub entries: Vec<PromptQueueEntry>,
    /// Legacy entries held back by the ownership migration (#461).
    ///
    /// Reported so a fail-closed migration is visible: a caller whose queue was
    /// quarantined sees a count rather than an empty queue that silently never
    /// runs.
    #[serde(default)]
    pub quarantined: usize,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptQueueTakeResult {
    pub batch: Option<PromptQueueBatch>,
    pub entries: Vec<PromptQueueEntry>,
    /// Turn reservation held for the drained batch, if any.
    ///
    /// Draining and starting the turn are two calls. Without a reservation
    /// another writer can start a turn in between, the start is refused, and
    /// the batch is already gone from the queue — a silently lost prompt. The
    /// drain therefore claims the session's turn slot under the same lock that
    /// removed the batch, and the caller must either present this owner when
    /// starting the turn or hand the batch back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptQueueRunNextResult {
    pub entries: Vec<PromptQueueEntry>,
    pub cancelled_active: bool,
    pub changed_entry: PromptQueueEntry,
}

/// What a `clear` actually stopped, so the receipt cannot overstate it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptQueueClearOutcome {
    /// Durable follow-ups removed.
    pub queued_cleared: usize,
    /// Accepted steering cancelled before it reached the model.
    pub steering_cancelled: usize,
    /// Steering already handed to a model boundary; clear cannot retract it,
    /// so it will still be injected. Non-zero means the session is not quiet.
    pub steering_in_flight: usize,
}

impl PromptQueueClearOutcome {
    /// True only when nothing survives the clear.
    pub fn fully_stopped(&self) -> bool {
        self.steering_in_flight == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SteeringDisposition {
    Pending,
    Queued,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteeringReceipt {
    pub entry: PromptQueueEntry,
    pub disposition: SteeringDisposition,
    pub entries: Vec<PromptQueueEntry>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct SessionPromptQueue {
    #[serde(default)]
    queued: VecDeque<PromptQueueEntry>,
    /// Mid-turn only; not meaningful after process restart (drained or deferred).
    #[serde(default)]
    steering: VecDeque<PromptQueueEntry>,
    /// Steering handed to the current model boundary but not yet acknowledged.
    #[serde(default)]
    delivering: VecDeque<PromptQueueEntry>,
}

impl SessionPromptQueue {
    /// Durable snapshot: queued follow-ups only (steering is process-local).
    pub fn durable_snapshot(&self) -> Self {
        let mut queued = self.queued.clone();
        // A crash cannot safely preserve mid-turn injection position. Persist an
        // equivalent high-priority follow-up so accepted steering is not lost.
        for mut entry in self.steering.iter().cloned().rev() {
            entry.source = "steering_deferred".into();
            entry.priority = true;
            entry.version += 1;
            queued.push_front(entry);
        }
        for mut entry in self.delivering.iter().cloned().rev() {
            entry.source = "steering_delivery_recovery".into();
            entry.priority = true;
            entry.version += 1;
            queued.push_front(entry);
        }
        Self {
            queued,
            steering: VecDeque::new(),
            delivering: VecDeque::new(),
        }
    }

    /// Every entry, in queue order, regardless of owner.
    ///
    /// This is the host's own authoritative view: persistence, the local
    /// desktop event stream, and turn delivery all need the true queue. It is
    /// never handed to a control-plane caller — those go through
    /// [`Self::list_for`].
    pub fn list(&self) -> Vec<PromptQueueEntry> {
        self.queued.iter().cloned().collect()
    }

    /// The entries `key` owns, in queue order.
    ///
    /// Quarantined legacy entries are owned by nobody and so appear for no
    /// caller. Positions are not renumbered: an entry's index in the full queue
    /// is what `to_index` and the revision fence are defined against, and
    /// hiding that would make a scoped reorder mean something different from
    /// the reorder that actually happens.
    pub fn list_for(&self, key: &QueueOwnerKey) -> Vec<PromptQueueEntry> {
        self.queued
            .iter()
            .filter(|entry| entry.owned_by(key))
            .cloned()
            .collect()
    }

    /// Number of quarantined legacy entries.
    ///
    /// Surfaced so a fail-closed migration is visible to an operator rather
    /// than looking like an empty queue.
    pub fn quarantined(&self) -> usize {
        self.queued
            .iter()
            .filter(|entry| entry.is_quarantined())
            .count()
    }

    /// Scoped lookup: the one place an id becomes an entry.
    ///
    /// Unknown, malformed, foreign, and quarantined ids all leave here as
    /// `None`, so every caller's refusal is produced by the same branch. This
    /// is what makes the four cases indistinguishable structurally rather than
    /// by keeping two error strings in sync.
    fn position_for(&self, id: &str, key: &QueueOwnerKey) -> Option<usize> {
        self.queued
            .iter()
            .position(|entry| entry.id == id && entry.owned_by(key))
    }

    pub fn add(
        &mut self,
        text: impl Into<String>,
        source: impl Into<String>,
        priority: bool,
        actor: &QueueActor,
    ) -> Result<PromptQueueEntry> {
        let entry = PromptQueueEntry::new(text, source, priority, actor)?;
        if priority {
            self.queued.push_front(entry.clone());
        } else {
            self.queued.push_back(entry.clone());
        }
        Ok(entry)
    }

    /// Edit an entry `key` owns.
    ///
    /// Ownership is resolved before the version is compared. Checking the
    /// version first would make `StaleVersion` a positive existence signal for
    /// another principal's entry — the caller would learn the entry exists, and
    /// by bisecting the version, what its version is.
    pub fn edit(
        &mut self,
        id: &str,
        version: u64,
        text: String,
        key: &QueueOwnerKey,
    ) -> Result<PromptQueueEntry> {
        let text = clean_prompt(text)?;
        let index = self
            .position_for(id, key)
            .ok_or_else(unknown_queued_prompt)?;
        let entry = &mut self.queued[index];
        if entry.version != version {
            bail!(
                "stale queued prompt version: expected {}, got {version}",
                entry.version
            );
        }
        entry.text = text;
        entry.kind = classify_kind(&entry.text).into();
        entry.version += 1;
        Ok(entry.clone())
    }

    /// Compare-and-set gate for every mutator that is not [`Self::edit`].
    ///
    /// The version is mandatory by type: an optional CAS on a control plane
    /// with two writers is last-write-wins, and callers reached for it exactly
    /// when they had no version to offer. This matches the Computer Use
    /// control fence, which requires the current version on every transition.
    pub fn check_version(&self, id: &str, version: u64, key: &QueueOwnerKey) -> Result<()> {
        let index = self
            .position_for(id, key)
            .ok_or_else(unknown_queued_prompt)?;
        let entry = &self.queued[index];
        if entry.version != version {
            bail!(
                "stale queued prompt version: expected {}, got {version}",
                entry.version
            );
        }
        Ok(())
    }

    pub fn remove(&mut self, id: &str, key: &QueueOwnerKey) -> Result<PromptQueueEntry> {
        let index = self
            .position_for(id, key)
            .ok_or_else(unknown_queued_prompt)?;
        Ok(self.queued.remove(index).expect("queue index exists"))
    }

    /// Stop everything this queue can still stop.
    ///
    /// `queued` and `steering` are both dropped: a coordinator calling clear
    /// wants the session to stop, and steering that has only been *accepted*
    /// has not reached the model yet, so cancelling it is honest. `delivering`
    /// has already been handed to a model boundary and cannot be retracted —
    /// it is reported in the outcome instead of being silently ignored, so no
    /// caller receives an "empty queue" receipt while an interjection is still
    /// on its way.
    /// Clear only what `key` owns.
    ///
    /// A principal-scoped clear is the only honest form once a queue can hold
    /// several principals' work: an unscoped clear would let any authenticated
    /// caller cancel every other principal's queued and accepted work, which is
    /// the destructive half of the very boundary this scoping exists to draw.
    /// Quarantined legacy entries are owned by nobody and so survive a clear —
    /// they are audit evidence, not this caller's to discard.
    pub fn clear(&mut self, key: &QueueOwnerKey) -> PromptQueueClearOutcome {
        let before = self.queued.len();
        self.queued.retain(|entry| !entry.owned_by(key));
        let queued_cleared = before - self.queued.len();

        let steering_before = self.steering.len();
        self.steering.retain(|entry| !entry.owned_by(key));
        let steering_cancelled = steering_before - self.steering.len();

        PromptQueueClearOutcome {
            queued_cleared,
            steering_cancelled,
            // Only this principal's unretractable steering is reported: a
            // caller must not learn that someone else has an interjection in
            // flight, and must not be told the session is noisy on another
            // principal's account.
            steering_in_flight: self
                .delivering
                .iter()
                .filter(|entry| entry.owned_by(key))
                .count(),
        }
    }

    /// Move an entry to an absolute index, bumping every version that shifted.
    ///
    /// `to_index` is absolute, so it only means anything against a specific
    /// ordering. Leaving versions untouched made `expected_version` unable to
    /// detect a conflict even when supplied: two coordinators reordering
    /// concurrently both succeeded and the final order was arbitrary. Every
    /// entry whose index changed now describes a different position than the
    /// one a concurrent writer read, so its version moves and that writer's
    /// CAS fails closed. Entries outside the moved span keep their versions,
    /// which keeps the conflict blast radius to the entries that actually
    /// shifted.
    pub fn move_to(&mut self, id: &str, to_index: usize, key: &QueueOwnerKey) -> Result<()> {
        let from_index = self
            .position_for(id, key)
            .ok_or_else(unknown_queued_prompt)?;
        let Some(entry) = self.queued.remove(from_index) else {
            return Err(unknown_queued_prompt());
        };
        let target = to_index.min(self.queued.len());
        self.queued.insert(target, entry);
        if target == from_index {
            // Nothing shifted, so nothing observed by another writer changed.
            return Ok(());
        }
        let (lo, hi) = if target < from_index {
            (target, from_index)
        } else {
            (from_index, target)
        };
        for entry in self.queued.range_mut(lo..=hi) {
            entry.version += 1;
        }
        Ok(())
    }

    pub fn run_next(&mut self, id: &str, key: &QueueOwnerKey) -> Result<PromptQueueEntry> {
        let mut entry = self.remove(id, key)?;
        entry.priority = true;
        entry.version += 1;
        self.queued.push_front(entry.clone());
        Ok(entry)
    }

    pub fn steer_queued(
        &mut self,
        id: &str,
        can_inject: bool,
        key: &QueueOwnerKey,
    ) -> Result<SteeringReceipt> {
        let mut entry = self.remove(id, key)?;
        if can_inject {
            entry.source = "steer_now".into();
            entry.version += 1;
            self.steering.push_back(entry.clone());
            Ok(SteeringReceipt {
                entry,
                disposition: SteeringDisposition::Pending,
                entries: self.list_for(key),
            })
        } else {
            entry.source = "steering_deferred".into();
            entry.priority = true;
            entry.version += 1;
            self.queued.push_front(entry.clone());
            Ok(SteeringReceipt {
                entry,
                disposition: SteeringDisposition::Queued,
                entries: self.list_for(key),
            })
        }
    }

    pub fn steer_text(
        &mut self,
        text: String,
        can_inject: bool,
        actor: &QueueActor,
    ) -> Result<SteeringReceipt> {
        let source = if can_inject {
            "steer_now"
        } else {
            "steering_deferred"
        };
        let entry = PromptQueueEntry::new(text, source, !can_inject, actor)?;
        if can_inject {
            self.steering.push_back(entry.clone());
        } else {
            self.queued.push_front(entry.clone());
        }
        Ok(SteeringReceipt {
            entry,
            disposition: if can_inject {
                SteeringDisposition::Pending
            } else {
                SteeringDisposition::Queued
            },
            entries: self.list_for(&actor.key()),
        })
    }

    /// Hand accepted steering of **exactly one owner** to the model boundary.
    ///
    /// Steering is injected into a running turn, so the same rule as
    /// [`Self::take_next`] applies: several principals' interjections handed to
    /// one boundary would execute as one unit and expose each other's text. The
    /// owner of the first deliverable interjection wins the boundary; the rest
    /// stay queued for the next one.
    ///
    /// Withheld steering is never dropped: authority may be restored (a
    /// credential reinstated, a workspace put back on the allowlist), and
    /// silently discarding accepted work would be a worse failure than
    /// deferring it.
    pub fn drain_steering(&mut self, gate: &DeliveryGate<'_>) -> Vec<PromptQueueEntry> {
        self.delivering.clear();
        let batch_owner = self
            .steering
            .iter()
            .find(|entry| gate.allows_owner(entry.owner_key.as_deref()))
            .and_then(|entry| entry.owner_key.clone());
        let Some(batch_owner) = batch_owner else {
            return Vec::new();
        };
        let mut withheld = VecDeque::new();
        for entry in std::mem::take(&mut self.steering) {
            let deliverable = entry.owner_key.as_deref() == Some(batch_owner.as_str())
                && gate.allows_owner(entry.owner_key.as_deref());
            if deliverable {
                self.delivering.push_back(entry);
            } else {
                withheld.push_back(entry);
            }
        }
        self.steering = withheld;
        self.delivering.iter().cloned().collect()
    }

    pub fn defer_pending_steering(&mut self) -> usize {
        self.delivering.clear();
        let pending: Vec<_> = self.steering.drain(..).collect();
        let count = pending.len();
        for mut entry in pending.into_iter().rev() {
            entry.source = "steering_deferred".into();
            entry.priority = true;
            entry.version += 1;
            self.queued.push_front(entry);
        }
        count
    }

    pub fn recover_pending_steering(&mut self) -> usize {
        let mut pending: Vec<_> = self.delivering.drain(..).collect();
        pending.extend(self.steering.drain(..));
        let count = pending.len();
        for mut entry in pending.into_iter().rev() {
            entry.source = "steering_delivery_recovery".into();
            entry.priority = true;
            entry.version += 1;
            self.queued.push_front(entry);
        }
        count
    }

    /// Drain the next batch for a turn: entries of **exactly one owner**, whose
    /// authority is still current.
    ///
    /// Two separate rules apply here, and conflating them was a real defect.
    ///
    /// *Authority* decides which entries are eligible at all. An entry may have
    /// been queued days ago under a credential that has since been removed, or
    /// against a workspace that has since left the allowlist; executing it then
    /// would be exactly the widening of authority that rotation prevents.
    ///
    /// *Ownership* decides which of the eligible entries may share one model
    /// boundary. A batch is joined into a single prompt, so mixing owners would
    /// concatenate one principal's prompt text into another's turn and let
    /// several principals execute as one unit with no delegation between them.
    /// The batch is therefore restricted to the owner of the first deliverable
    /// entry — queue order picks the owner, so no principal can starve another,
    /// and each drain removes that owner's head entries.
    ///
    /// Entries of other owners are stepped over rather than blocking the queue
    /// head. That cannot reorder anything an observer relies on: no principal
    /// can see another's entries, so the only ordering any caller can observe —
    /// its own — is preserved.
    pub fn take_next(&mut self, gate: &DeliveryGate<'_>) -> PromptQueueTakeResult {
        let Some(batch_owner) = self
            .queued
            .iter()
            .find(|entry| gate.allows_owner(entry.owner_key.as_deref()))
            .and_then(|entry| entry.owner_key.clone())
        else {
            return PromptQueueTakeResult {
                batch: None,
                entries: Vec::new(),
                reservation: None,
            };
        };
        let deliverable: Vec<usize> = self
            .queued
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.owner_key.as_deref() == Some(batch_owner.as_str())
                    && gate.allows_owner(entry.owner_key.as_deref())
            })
            .map(|(index, _)| index)
            .collect();
        let gates = deliverable.iter().map(|index| {
            let entry = &self.queued[*index];
            CombineGate {
                id: &entry.id,
                is_plain_prompt: entry.kind == "prompt" && !entry.priority,
                is_synthetic: false,
                is_expanded_skill: false,
                is_bash: entry.kind == "command" && entry.text.trim_start().starts_with('!'),
                has_images: false,
                text: &entry.text,
            }
        });
        let count = combine_prefix_len(gates, &[]).max(1).min(deliverable.len());
        // Remove from the back so earlier indices stay valid.
        let mut drained = Vec::with_capacity(count);
        for index in deliverable.iter().take(count).rev() {
            if let Some(entry) = self.queued.remove(*index) {
                drained.push(entry);
            }
        }
        drained.reverse();
        let text = join_texts(drained.iter().map(|entry| entry.text.as_str()));
        PromptQueueTakeResult {
            batch: Some(PromptQueueBatch {
                entries: drained,
                text,
            }),
            entries: self.list(),
            // The host attaches the reservation; the queue itself owns no
            // turn state.
            reservation: None,
        }
    }

    /// Put a drained batch back at the head, in its original order.
    ///
    /// Used when the turn the batch was drained for never started. Versions
    /// are bumped because the entries left the queue and came back: any holder
    /// of the pre-drain version was, for a moment, describing an entry that no
    /// longer existed.
    pub fn restore_batch(&mut self, entries: Vec<PromptQueueEntry>) {
        for mut entry in entries.into_iter().rev() {
            entry.version += 1;
            self.queued.push_front(entry);
        }
    }
}

fn clean_prompt(text: String) -> Result<String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        bail!("prompt cannot be empty");
    }
    if text.len() > MAX_PROMPT_BYTES {
        bail!("prompt exceeds {MAX_PROMPT_BYTES} bytes");
    }
    Ok(text)
}

fn classify_kind(text: &str) -> &'static str {
    let text = text.trim_start();
    if text.starts_with('/') || text.starts_with('!') {
        "command"
    } else {
        "prompt"
    }
}

pub(crate) fn format_interjection(text: &str) -> String {
    let text = if text.len() > LARGE_STEERING_BYTES {
        let end = text
            .char_indices()
            .take_while(|(index, _)| *index < LARGE_STEERING_BYTES)
            .last()
            .map(|(index, ch)| index + ch.len_utf8())
            .unwrap_or(text.len());
        format!("{}... [truncated]", &text[..end])
    } else {
        text.to_string()
    };
    format!("The user sent a message while you were working:\n<user_query>\n{text}\n</user_query>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue_authority::{QueueAuthority, QueuePrincipal};
    use uuid::Uuid;

    fn session() -> Uuid {
        Uuid::from_u128(0x9111)
    }

    /// The principal these unit tests act as.
    fn actor() -> QueueActor {
        QueueActor::desktop(session(), "/w")
    }

    /// The compatibility control-plane principal, whose wire owner value is
    /// `mcp`.
    fn control_actor() -> QueueActor {
        QueueActor::new(
            QueuePrincipal::control(
                "acct",
                crate::queue_authority::CONTROL_PRINCIPAL,
                session(),
                "/w",
            ),
            QueueProvenance::default(),
        )
    }

    /// A second, distinct principal — the adversary in the ownership tests.
    fn other_actor() -> QueueActor {
        QueueActor::new(
            QueuePrincipal::control("acct", "intruder", session(), "/w"),
            QueueProvenance::default(),
        )
    }

    /// Delivery gate over `authority` for this test session and workspace.
    fn gate_for(authority: &QueueAuthority) -> DeliveryGate<'_> {
        DeliveryGate::new(authority, session(), "/w")
    }

    /// Authority in which the control principal is live, for tests that steer
    /// as `mcp` rather than as the desktop.
    fn control_authority() -> QueueAuthority {
        QueueAuthority::control(
            "acct",
            [crate::queue_authority::CONTROL_PRINCIPAL.to_string()],
            ["/w".to_string()],
            QueueProvenance::default(),
        )
        .expect("valid test authority")
    }

    // ── Principal ownership (#461) ──────────────────────────────────────

    #[test]
    fn a_foreign_principal_cannot_see_or_touch_an_entry() {
        let mut queue = SessionPromptQueue::default();
        let entry = queue.add("mine", "composer", false, &actor()).unwrap();
        let intruder = other_actor().key();

        assert!(
            queue.list_for(&intruder).is_empty(),
            "a foreign principal must not see the entry"
        );
        assert_eq!(queue.list_for(&actor().key()).len(), 1);

        // Every mutator refuses, and all of them produce the one refusal.
        for message in [
            queue
                .clone()
                .edit(&entry.id, entry.version, "hijack".into(), &intruder)
                .unwrap_err()
                .to_string(),
            queue
                .clone()
                .check_version(&entry.id, entry.version, &intruder)
                .unwrap_err()
                .to_string(),
            queue
                .clone()
                .remove(&entry.id, &intruder)
                .unwrap_err()
                .to_string(),
            queue
                .clone()
                .move_to(&entry.id, 0, &intruder)
                .unwrap_err()
                .to_string(),
            queue
                .clone()
                .run_next(&entry.id, &intruder)
                .unwrap_err()
                .to_string(),
            queue
                .clone()
                .steer_queued(&entry.id, true, &intruder)
                .unwrap_err()
                .to_string(),
        ] {
            assert_eq!(message, "unknown queued prompt");
        }

        // Nothing moved.
        assert_eq!(queue.list().len(), 1);
        assert_eq!(queue.list()[0].version, entry.version);
    }

    #[test]
    fn a_foreign_entry_never_reaches_the_stale_version_branch() {
        // Checking the version before ownership would make `StaleVersion` a
        // positive existence signal for another principal's entry.
        let mut queue = SessionPromptQueue::default();
        let entry = queue.add("mine", "composer", false, &actor()).unwrap();
        let intruder = other_actor().key();
        let wrong_version = entry.version + 41;

        let foreign = queue
            .check_version(&entry.id, wrong_version, &intruder)
            .unwrap_err()
            .to_string();
        let unknown = queue
            .check_version("no-such-entry", wrong_version, &intruder)
            .unwrap_err()
            .to_string();
        assert_eq!(foreign, unknown);
        assert_eq!(foreign, "unknown queued prompt");

        // The owner's own stale version still reports usefully.
        let own = queue
            .check_version(&entry.id, wrong_version, &actor().key())
            .unwrap_err()
            .to_string();
        assert!(own.starts_with("stale queued prompt version"), "{own}");
        assert!(
            !own.contains(&entry.id),
            "even an owner's refusal need not echo the id: {own}"
        );
    }

    #[test]
    fn clear_is_scoped_to_the_calling_principal() {
        let mut queue = SessionPromptQueue::default();
        queue.add("mine", "composer", false, &actor()).unwrap();
        queue
            .add("theirs", "composer", false, &other_actor())
            .unwrap();

        let outcome = queue.clear(&actor().key());
        assert_eq!(outcome.queued_cleared, 1);
        assert_eq!(
            queue.list().len(),
            1,
            "another principal's queued work must survive a clear"
        );
        assert_eq!(queue.list()[0].text, "theirs");
    }

    #[test]
    fn clear_reports_only_the_callers_own_in_flight_steering() {
        let mut queue = SessionPromptQueue::default();
        queue
            .steer_text("theirs".into(), true, &other_actor())
            .unwrap();
        let authority = QueueAuthority::control(
            "acct",
            [
                "intruder".to_string(),
                crate::queue_authority::CONTROL_PRINCIPAL.to_string(),
            ],
            ["/w".to_string()],
            QueueProvenance::default(),
        )
        .expect("valid test authority");
        assert_eq!(queue.drain_steering(&gate_for(&authority)).len(), 1);

        let outcome = queue.clear(&actor().key());
        assert_eq!(
            outcome.steering_in_flight, 0,
            "a caller must not be told another principal has an interjection in flight"
        );
        assert!(outcome.fully_stopped());
    }

    // ── Delivery must not mix principals (#461 P0) ──────────────────────

    /// An authority in which *both* test principals are live at once.
    ///
    /// The revocation tests deliberately take one principal away before
    /// draining, which is exactly the case that cannot catch mixing. This is
    /// the case that can.
    fn two_live_authority() -> QueueAuthority {
        QueueAuthority::control(
            "acct",
            [
                "intruder".to_string(),
                crate::queue_authority::CONTROL_PRINCIPAL.to_string(),
            ],
            ["/w".to_string()],
            QueueProvenance::default(),
        )
        .expect("valid test authority")
    }

    #[test]
    fn a_drain_never_mixes_two_principals_into_one_turn() {
        let mut queue = SessionPromptQueue::default();
        queue
            .add("control work", "composer", false, &control_actor())
            .unwrap();
        queue
            .add("intruder secret", "composer", false, &other_actor())
            .unwrap();

        let authority = two_live_authority();
        let batch = queue
            .take_next(&gate_for(&authority))
            .batch
            .expect("something is deliverable");

        let owners: std::collections::BTreeSet<_> = batch
            .entries
            .iter()
            .map(|entry| entry.owner_key.clone())
            .collect();
        assert_eq!(
            owners.len(),
            1,
            "a single model boundary must carry exactly one owner, got {owners:?}"
        );
        assert!(
            !(batch.text.contains("control work") && batch.text.contains("intruder secret")),
            "one principal's prompt text must never be concatenated with another's: {:?}",
            batch.text
        );
    }

    #[test]
    fn a_steering_drain_never_mixes_two_principals() {
        let mut queue = SessionPromptQueue::default();
        queue
            .steer_text("control interjection".into(), true, &control_actor())
            .unwrap();
        queue
            .steer_text("intruder interjection".into(), true, &other_actor())
            .unwrap();

        let authority = two_live_authority();
        let delivered = queue.drain_steering(&gate_for(&authority));
        let owners: std::collections::BTreeSet<_> = delivered
            .iter()
            .map(|entry| entry.owner_key.clone())
            .collect();
        assert!(
            owners.len() <= 1,
            "one steering boundary must carry at most one owner, got {owners:?}"
        );
    }

    // ── Legacy migration (#461) ─────────────────────────────────────────

    /// A queue exactly as a pre-#461 build persisted it: entries with an
    /// `owner` label but no ownership handle.
    fn legacy_queue_json() -> &'static str {
        r#"{
            "queued": [
                {
                    "id": "11111111-1111-1111-1111-111111111111",
                    "version": 0,
                    "text": "legacy follow-up",
                    "kind": "prompt",
                    "source": "control",
                    "owner": "mcp",
                    "created_at": "2026-01-01T00:00:00Z",
                    "priority": false
                }
            ],
            "steering": [],
            "delivering": []
        }"#
    }

    #[test]
    fn a_legacy_queue_still_deserializes() {
        let queue: SessionPromptQueue = serde_json::from_str(legacy_queue_json()).unwrap();
        assert_eq!(queue.list().len(), 1, "the entry must not be dropped");
        assert!(
            queue.list()[0].is_quarantined(),
            "a legacy entry has no ownership handle"
        );
        assert_eq!(queue.quarantined(), 1);
    }

    #[test]
    fn a_legacy_entry_is_owned_by_nobody() {
        let queue: SessionPromptQueue = serde_json::from_str(legacy_queue_json()).unwrap();
        let legacy_id = queue.list()[0].id.clone();

        // Not even the principal whose wire label it carries can claim it: the
        // `mcp` label was shared by every credential, so adopting it would be
        // exactly the silent sharing this migration refuses.
        for key in [actor().key(), other_actor().key(), control_actor().key()] {
            assert!(queue.list_for(&key).is_empty());
            assert!(queue
                .clone()
                .remove(&legacy_id, &key)
                .unwrap_err()
                .to_string()
                .eq("unknown queued prompt"));
        }
    }

    #[test]
    fn a_legacy_entry_is_never_delivered_but_is_never_deleted() {
        let mut queue: SessionPromptQueue = serde_json::from_str(legacy_queue_json()).unwrap();
        let authority = control_authority();
        assert!(
            queue.take_next(&gate_for(&authority)).batch.is_none(),
            "a principal-less entry must not be executed"
        );
        assert_eq!(
            queue.list().len(),
            1,
            "quarantine must retain the entry as audit evidence"
        );
        // It also survives another principal's clear.
        queue.clear(&control_actor().key());
        assert_eq!(queue.list().len(), 1);
    }

    #[test]
    fn a_quarantined_entry_does_not_block_deliverable_work_behind_it() {
        let mut queue: SessionPromptQueue = serde_json::from_str(legacy_queue_json()).unwrap();
        queue
            .add("live work", "composer", false, &control_actor())
            .unwrap();
        let authority = control_authority();
        let taken = queue
            .take_next(&gate_for(&authority))
            .batch
            .expect("the live entry is deliverable");
        assert_eq!(taken.entries.len(), 1);
        assert_eq!(taken.entries[0].text, "live work");
        assert_eq!(
            queue.quarantined(),
            1,
            "the quarantined entry stays put rather than being consumed"
        );
    }

    #[test]
    fn queue_mutations_are_versioned_and_ordered() {
        let mut queue = SessionPromptQueue::default();
        let a = queue.add("one", "composer", false, &actor()).unwrap();
        let b = queue.add("two", "composer", false, &actor()).unwrap();
        let c = queue.add("three", "composer", false, &actor()).unwrap();

        let edited = queue
            .edit(&b.id, 0, "/help".into(), &actor().key())
            .unwrap();
        assert_eq!(edited.version, 1);
        assert_eq!(edited.kind, "command");
        assert!(queue
            .edit(&b.id, 0, "stale".into(), &actor().key())
            .is_err());

        queue.move_to(&c.id, 0, &actor().key()).unwrap();
        assert_eq!(
            queue
                .list()
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            [&c.id, &a.id, &b.id]
        );
        queue.remove(&a.id, &actor().key()).unwrap();
        queue.clear(&actor().key());
        assert!(queue.list().is_empty());
    }

    /// S3: `to_index` is absolute, so it only means something against a known
    /// ordering. Reorder used to leave every version alone, which made the
    /// conflict structurally undetectable — `expected_version` could not catch
    /// it even when supplied. Everything that shifted must move its version.
    #[test]
    fn reorder_bumps_the_version_of_every_entry_that_shifted() {
        let mut queue = SessionPromptQueue::default();
        for text in ["a", "b", "c", "d"] {
            queue.add(text, "composer", false, &actor()).unwrap();
        }
        let before: Vec<u64> = queue.list().iter().map(|entry| entry.version).collect();
        assert_eq!(before, vec![0, 0, 0, 0]);

        let d_id = queue.list()[3].id.clone();
        queue.move_to(&d_id, 1, &actor().key()).unwrap();

        let after = queue.list();
        assert_eq!(
            after.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            vec!["a", "d", "b", "c"]
        );
        // "a" never moved, so a concurrent writer holding its version is still
        // describing the entry accurately and must not be spuriously rejected.
        assert_eq!(after[0].version, 0, "unshifted entry keeps its version");
        assert_eq!(after[1].version, 1, "the moved entry shifted");
        assert_eq!(after[2].version, 1, "displaced entry shifted");
        assert_eq!(after[3].version, 1, "displaced entry shifted");
    }

    /// A move that lands where it started shifts nothing, so it must not
    /// invalidate versions another coordinator is legitimately holding.
    #[test]
    fn reorder_to_the_same_index_leaves_every_version_alone() {
        let mut queue = SessionPromptQueue::default();
        for text in ["a", "b", "c"] {
            queue.add(text, "composer", false, &actor()).unwrap();
        }
        let b_id = queue.list()[1].id.clone();
        queue.move_to(&b_id, 1, &actor().key()).unwrap();
        assert!(queue.list().iter().all(|entry| entry.version == 0));
    }

    /// S3: the case the review named. Two coordinators read the same queue and
    /// both reorder. The first wins; the second is describing an ordering that
    /// no longer exists, so its CAS has to fail rather than silently producing
    /// an arbitrary final order with two success receipts.
    #[test]
    fn a_second_concurrent_reorder_fails_its_compare_and_set() {
        let mut queue = SessionPromptQueue::default();
        for text in ["a", "b", "c"] {
            queue.add(text, "composer", false, &actor()).unwrap();
        }
        let listed = queue.list();
        let (a_id, b_id) = (listed[0].id.clone(), listed[1].id.clone());
        // Both coordinators read version 0 for the entry each intends to move.
        let (a_version, b_version) = (listed[0].version, listed[1].version);

        // Coordinator A wins: move "a" to the back.
        queue
            .check_version(&a_id, a_version, &actor().key())
            .unwrap();
        queue.move_to(&a_id, 2, &actor().key()).unwrap();

        // Coordinator B is still working from the pre-move ordering.
        let conflict = queue
            .check_version(&b_id, b_version, &actor().key())
            .unwrap_err();
        assert!(
            conflict.to_string().contains("stale queued prompt version"),
            "expected a stale-version conflict, got: {conflict}"
        );
        assert_eq!(
            queue
                .list()
                .iter()
                .map(|e| e.text.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "c", "a"],
            "the losing reorder must not have been applied"
        );
    }

    /// S3: an omitted version used to mean "no check". The type no longer
    /// admits that, so this asserts the remaining half — a version that is
    /// merely wrong is rejected on every mutator that takes one.
    #[test]
    fn check_version_rejects_a_wrong_version() {
        let mut queue = SessionPromptQueue::default();
        let entry = queue.add("only", "composer", false, &actor()).unwrap();
        queue
            .check_version(&entry.id, entry.version, &actor().key())
            .unwrap();
        assert!(queue
            .check_version(&entry.id, entry.version + 1, &actor().key())
            .is_err());
        assert!(queue
            .check_version("no-such-entry", 0, &actor().key())
            .is_err());
    }

    /// S4: `clear` used to leave `steering` untouched, so a coordinator that
    /// called it to stop the session got an empty-queue receipt while an
    /// accepted interjection was still waiting to be injected.
    #[test]
    fn clear_cancels_accepted_steering_that_has_not_been_delivered() {
        let mut queue = SessionPromptQueue::default();
        queue.add("follow up", "composer", false, &actor()).unwrap();
        queue
            .steer_text("change direction".into(), true, &actor())
            .unwrap();

        let outcome = queue.clear(&actor().key());
        assert_eq!(outcome.queued_cleared, 1);
        assert_eq!(outcome.steering_cancelled, 1);
        assert_eq!(outcome.steering_in_flight, 0);
        assert!(outcome.fully_stopped());

        assert!(queue.list().is_empty());
        // The cancelled steering must not resurface at the next boundary, on
        // the deferral path, or through durable recovery.
        assert!(queue
            .drain_steering(&gate_for(&QueueAuthority::default()))
            .is_empty());
        assert_eq!(queue.defer_pending_steering(), 0);
        assert_eq!(queue.recover_pending_steering(), 0);
        assert!(queue.durable_snapshot().list().is_empty());
    }

    /// Steering already handed to a model boundary cannot be retracted, so the
    /// receipt must say so instead of reporting a quiet session.
    #[test]
    fn clear_reports_in_flight_steering_rather_than_claiming_it_stopped() {
        let mut queue = SessionPromptQueue::default();
        queue
            .steer_text("already delivered".into(), true, &actor())
            .unwrap();
        assert_eq!(
            queue
                .drain_steering(&gate_for(&QueueAuthority::default()))
                .len(),
            1
        );

        let outcome = queue.clear(&actor().key());
        assert_eq!(outcome.steering_cancelled, 0);
        assert_eq!(outcome.steering_in_flight, 1);
        assert!(
            !outcome.fully_stopped(),
            "an unretractable interjection must not read as stopped"
        );
        assert!(queue.list().is_empty());
    }

    #[test]
    fn take_next_combines_only_plain_nonpriority_prefix() {
        let mut queue = SessionPromptQueue::default();
        queue.add("one", "composer", false, &actor()).unwrap();
        queue.add("two", "composer", false, &actor()).unwrap();
        queue.add("/help", "composer", false, &actor()).unwrap();

        let first = queue.take_next(&gate_for(&QueueAuthority::default()));
        assert_eq!(first.batch.unwrap().text, "one\n\ntwo");
        let second = queue.take_next(&gate_for(&QueueAuthority::default()));
        assert_eq!(second.batch.unwrap().text, "/help");
        assert!(second.entries.is_empty());
    }

    #[test]
    fn steering_drains_exactly_once_or_defers_at_boundary() {
        let mut queue = SessionPromptQueue::default();
        let receipt = queue
            .steer_text("change direction".into(), true, &actor())
            .unwrap();
        assert_eq!(receipt.disposition, SteeringDisposition::Pending);
        assert_eq!(
            queue
                .drain_steering(&gate_for(&QueueAuthority::default()))
                .len(),
            1
        );
        assert!(queue
            .drain_steering(&gate_for(&QueueAuthority::default()))
            .is_empty());

        queue
            .steer_text("late steer".into(), true, &actor())
            .unwrap();
        assert_eq!(queue.defer_pending_steering(), 1);
        assert_eq!(queue.list().len(), 1);
        assert_eq!(queue.list()[0].source, "steering_deferred");
        assert!(queue
            .drain_steering(&gate_for(&QueueAuthority::default()))
            .is_empty());
    }

    #[test]
    fn in_flight_steering_remains_durably_recoverable_until_acknowledged() {
        let mut queue = SessionPromptQueue::default();
        let receipt = queue
            .steer_text("keep this".into(), true, &actor())
            .expect("steer");
        assert_eq!(
            queue
                .drain_steering(&gate_for(&QueueAuthority::default()))
                .len(),
            1
        );
        let recovery = queue.durable_snapshot().list();
        assert_eq!(recovery.len(), 1);
        assert_eq!(recovery[0].id, receipt.entry.id);
        assert_eq!(recovery[0].source, "steering_delivery_recovery");

        assert!(queue
            .drain_steering(&gate_for(&QueueAuthority::default()))
            .is_empty());
        assert!(queue.durable_snapshot().list().is_empty());
    }

    #[test]
    fn multiple_steering_entries_keep_fifo_order_through_delivery_recovery() {
        let authority = control_authority();
        let mut queue = SessionPromptQueue::default();
        let first = queue
            .steer_text("first direction".into(), true, &control_actor())
            .unwrap();
        let second = queue
            .steer_text("second direction".into(), true, &control_actor())
            .unwrap();

        let delivered = queue.drain_steering(&gate_for(&authority));
        assert_eq!(
            delivered
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            vec![first.entry.id.as_str(), second.entry.id.as_str()]
        );

        let recovered = queue.durable_snapshot().list();
        assert_eq!(
            recovered
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            vec!["first direction", "second direction"]
        );
        assert!(recovered.iter().all(|entry| entry.priority));
        assert_eq!(recovered[0].source, "steering_delivery_recovery");
        assert_eq!(recovered[1].owner.as_deref(), Some("mcp"));
    }

    #[test]
    fn durable_snapshot_defers_pending_steering_with_owner() {
        let mut queue = SessionPromptQueue::default();
        queue
            .steer_text("focus".into(), true, &control_actor())
            .unwrap();
        let durable = queue.durable_snapshot();
        let entry = &durable.list()[0];
        assert_eq!(entry.source, "steering_deferred");
        assert_eq!(entry.owner.as_deref(), Some("mcp"));
        assert!(entry.priority);
    }

    #[test]
    fn run_next_is_priority_and_does_not_combine() {
        let mut queue = SessionPromptQueue::default();
        let a = queue.add("first", "composer", false, &actor()).unwrap();
        queue.add("second", "composer", false, &actor()).unwrap();
        queue.run_next(&a.id, &actor().key()).unwrap();

        let result = queue.take_next(&gate_for(&QueueAuthority::default()));
        assert_eq!(result.batch.unwrap().entries.len(), 1);
        assert_eq!(result.entries.len(), 1);
    }

    #[test]
    fn interjection_format_matches_parent_semantics() {
        let formatted = format_interjection("fix the test first");
        assert!(formatted
            .starts_with("The user sent a message while you were working:\n<user_query>\n"));
        assert!(formatted.ends_with("\n</user_query>"));
    }
}
