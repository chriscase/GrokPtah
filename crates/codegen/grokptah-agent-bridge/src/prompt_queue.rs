//! Bridge-owned prompt queue and mid-turn steering primitives (#191).

use std::collections::VecDeque;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::prompt_combine::{combine_prefix_len, join_texts, CombineGate};

const MAX_PROMPT_BYTES: usize = 100_000;
const LARGE_STEERING_BYTES: usize = 25_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptQueueEntry {
    pub id: String,
    pub version: u64,
    pub text: String,
    pub kind: String,
    pub source: String,
    pub owner: Option<String>,
    pub created_at: DateTime<Utc>,
    pub priority: bool,
}

impl PromptQueueEntry {
    pub fn new(text: impl Into<String>, source: impl Into<String>, priority: bool) -> Result<Self> {
        let text = clean_prompt(text.into())?;
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            version: 0,
            kind: classify_kind(&text).into(),
            text,
            source: source.into(),
            owner: Some("desktop".into()),
            created_at: Utc::now(),
            priority,
        })
    }
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

    pub fn list(&self) -> Vec<PromptQueueEntry> {
        self.queued.iter().cloned().collect()
    }

    #[allow(dead_code)]
    pub fn add(
        &mut self,
        text: impl Into<String>,
        source: impl Into<String>,
        priority: bool,
    ) -> Result<PromptQueueEntry> {
        self.add_with_owner(text, source, priority, Some("desktop".into()))
    }

    pub fn add_with_owner(
        &mut self,
        text: impl Into<String>,
        source: impl Into<String>,
        priority: bool,
        owner: Option<String>,
    ) -> Result<PromptQueueEntry> {
        let mut entry = PromptQueueEntry::new(text, source, priority)?;
        entry.owner = owner;
        if priority {
            self.queued.push_front(entry.clone());
        } else {
            self.queued.push_back(entry.clone());
        }
        Ok(entry)
    }

    pub fn edit(&mut self, id: &str, version: u64, text: String) -> Result<PromptQueueEntry> {
        let text = clean_prompt(text)?;
        let entry = self
            .queued
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown queued prompt {id}"))?;
        if entry.version != version {
            bail!(
                "stale queued prompt version for {id}: expected {}, got {version}",
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
    pub fn check_version(&self, id: &str, version: u64) -> Result<()> {
        let entry = self
            .queued
            .iter()
            .find(|entry| entry.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown queued prompt {id}"))?;
        if entry.version != version {
            bail!(
                "stale queued prompt version for {id}: expected {}, got {version}",
                entry.version
            );
        }
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<PromptQueueEntry> {
        let index = self
            .queued
            .iter()
            .position(|entry| entry.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown queued prompt {id}"))?;
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
    pub fn clear(&mut self) -> PromptQueueClearOutcome {
        let queued_cleared = self.queued.len();
        let steering_cancelled = self.steering.len();
        self.queued.clear();
        self.steering.clear();
        PromptQueueClearOutcome {
            queued_cleared,
            steering_cancelled,
            steering_in_flight: self.delivering.len(),
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
    pub fn move_to(&mut self, id: &str, to_index: usize) -> Result<()> {
        let from_index = self
            .queued
            .iter()
            .position(|entry| entry.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown queued prompt {id}"))?;
        let Some(entry) = self.queued.remove(from_index) else {
            bail!("unknown queued prompt {id}");
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

    pub fn run_next(&mut self, id: &str) -> Result<PromptQueueEntry> {
        let mut entry = self.remove(id)?;
        entry.priority = true;
        entry.version += 1;
        self.queued.push_front(entry.clone());
        Ok(entry)
    }

    pub fn steer_queued(&mut self, id: &str, can_inject: bool) -> Result<SteeringReceipt> {
        let mut entry = self.remove(id)?;
        if can_inject {
            entry.source = "steer_now".into();
            entry.version += 1;
            self.steering.push_back(entry.clone());
            Ok(SteeringReceipt {
                entry,
                disposition: SteeringDisposition::Pending,
                entries: self.list(),
            })
        } else {
            entry.source = "steering_deferred".into();
            entry.priority = true;
            entry.version += 1;
            self.queued.push_front(entry.clone());
            Ok(SteeringReceipt {
                entry,
                disposition: SteeringDisposition::Queued,
                entries: self.list(),
            })
        }
    }

    #[allow(dead_code)]
    pub fn steer_text(&mut self, text: String, can_inject: bool) -> Result<SteeringReceipt> {
        self.steer_text_with_owner(text, can_inject, Some("desktop".into()))
    }

    pub fn steer_text_with_owner(
        &mut self,
        text: String,
        can_inject: bool,
        owner: Option<String>,
    ) -> Result<SteeringReceipt> {
        let source = if can_inject {
            "steer_now"
        } else {
            "steering_deferred"
        };
        let mut entry = PromptQueueEntry::new(text, source, !can_inject)?;
        entry.owner = owner;
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
            entries: self.list(),
        })
    }

    pub fn drain_steering(&mut self) -> Vec<PromptQueueEntry> {
        self.delivering.clear();
        self.delivering.extend(self.steering.drain(..));
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

    pub fn take_next(&mut self) -> PromptQueueTakeResult {
        if self.queued.is_empty() {
            return PromptQueueTakeResult {
                batch: None,
                entries: Vec::new(),
                reservation: None,
            };
        }
        let gates = self.queued.iter().map(|entry| CombineGate {
            id: &entry.id,
            is_plain_prompt: entry.kind == "prompt" && !entry.priority,
            is_synthetic: false,
            is_expanded_skill: false,
            is_bash: entry.kind == "command" && entry.text.trim_start().starts_with('!'),
            has_images: false,
            text: &entry.text,
        });
        let count = combine_prefix_len(gates, &[]).max(1);
        let mut drained = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(entry) = self.queued.pop_front() {
                drained.push(entry);
            }
        }
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

    #[test]
    fn queue_mutations_are_versioned_and_ordered() {
        let mut queue = SessionPromptQueue::default();
        let a = queue.add("one", "composer", false).unwrap();
        let b = queue.add("two", "composer", false).unwrap();
        let c = queue.add("three", "composer", false).unwrap();

        let edited = queue.edit(&b.id, 0, "/help".into()).unwrap();
        assert_eq!(edited.version, 1);
        assert_eq!(edited.kind, "command");
        assert!(queue.edit(&b.id, 0, "stale".into()).is_err());

        queue.move_to(&c.id, 0).unwrap();
        assert_eq!(
            queue
                .list()
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            [&c.id, &a.id, &b.id]
        );
        queue.remove(&a.id).unwrap();
        queue.clear();
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
            queue.add(text, "composer", false).unwrap();
        }
        let before: Vec<u64> = queue.list().iter().map(|entry| entry.version).collect();
        assert_eq!(before, vec![0, 0, 0, 0]);

        let d_id = queue.list()[3].id.clone();
        queue.move_to(&d_id, 1).unwrap();

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
            queue.add(text, "composer", false).unwrap();
        }
        let b_id = queue.list()[1].id.clone();
        queue.move_to(&b_id, 1).unwrap();
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
            queue.add(text, "composer", false).unwrap();
        }
        let listed = queue.list();
        let (a_id, b_id) = (listed[0].id.clone(), listed[1].id.clone());
        // Both coordinators read version 0 for the entry each intends to move.
        let (a_version, b_version) = (listed[0].version, listed[1].version);

        // Coordinator A wins: move "a" to the back.
        queue.check_version(&a_id, a_version).unwrap();
        queue.move_to(&a_id, 2).unwrap();

        // Coordinator B is still working from the pre-move ordering.
        let conflict = queue.check_version(&b_id, b_version).unwrap_err();
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
        let entry = queue.add("only", "composer", false).unwrap();
        queue.check_version(&entry.id, entry.version).unwrap();
        assert!(queue.check_version(&entry.id, entry.version + 1).is_err());
        assert!(queue.check_version("no-such-entry", 0).is_err());
    }

    /// S4: `clear` used to leave `steering` untouched, so a coordinator that
    /// called it to stop the session got an empty-queue receipt while an
    /// accepted interjection was still waiting to be injected.
    #[test]
    fn clear_cancels_accepted_steering_that_has_not_been_delivered() {
        let mut queue = SessionPromptQueue::default();
        queue.add("follow up", "composer", false).unwrap();
        queue
            .steer_text_with_owner("change direction".into(), true, Some("mcp".into()))
            .unwrap();

        let outcome = queue.clear();
        assert_eq!(outcome.queued_cleared, 1);
        assert_eq!(outcome.steering_cancelled, 1);
        assert_eq!(outcome.steering_in_flight, 0);
        assert!(outcome.fully_stopped());

        assert!(queue.list().is_empty());
        // The cancelled steering must not resurface at the next boundary, on
        // the deferral path, or through durable recovery.
        assert!(queue.drain_steering().is_empty());
        assert_eq!(queue.defer_pending_steering(), 0);
        assert_eq!(queue.recover_pending_steering(), 0);
        assert!(queue.durable_snapshot().list().is_empty());
    }

    /// Steering already handed to a model boundary cannot be retracted, so the
    /// receipt must say so instead of reporting a quiet session.
    #[test]
    fn clear_reports_in_flight_steering_rather_than_claiming_it_stopped() {
        let mut queue = SessionPromptQueue::default();
        queue.steer_text("already delivered".into(), true).unwrap();
        assert_eq!(queue.drain_steering().len(), 1);

        let outcome = queue.clear();
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
        queue.add("one", "composer", false).unwrap();
        queue.add("two", "composer", false).unwrap();
        queue.add("/help", "composer", false).unwrap();

        let first = queue.take_next();
        assert_eq!(first.batch.unwrap().text, "one\n\ntwo");
        let second = queue.take_next();
        assert_eq!(second.batch.unwrap().text, "/help");
        assert!(second.entries.is_empty());
    }

    #[test]
    fn steering_drains_exactly_once_or_defers_at_boundary() {
        let mut queue = SessionPromptQueue::default();
        let receipt = queue.steer_text("change direction".into(), true).unwrap();
        assert_eq!(receipt.disposition, SteeringDisposition::Pending);
        assert_eq!(queue.drain_steering().len(), 1);
        assert!(queue.drain_steering().is_empty());

        queue.steer_text("late steer".into(), true).unwrap();
        assert_eq!(queue.defer_pending_steering(), 1);
        assert_eq!(queue.list().len(), 1);
        assert_eq!(queue.list()[0].source, "steering_deferred");
        assert!(queue.drain_steering().is_empty());
    }

    #[test]
    fn in_flight_steering_remains_durably_recoverable_until_acknowledged() {
        let mut queue = SessionPromptQueue::default();
        let receipt = queue.steer_text("keep this".into(), true).expect("steer");
        assert_eq!(queue.drain_steering().len(), 1);
        let recovery = queue.durable_snapshot().list();
        assert_eq!(recovery.len(), 1);
        assert_eq!(recovery[0].id, receipt.entry.id);
        assert_eq!(recovery[0].source, "steering_delivery_recovery");

        assert!(queue.drain_steering().is_empty());
        assert!(queue.durable_snapshot().list().is_empty());
    }

    #[test]
    fn multiple_steering_entries_keep_fifo_order_through_delivery_recovery() {
        let mut queue = SessionPromptQueue::default();
        let first = queue
            .steer_text_with_owner("first direction".into(), true, Some("desktop".into()))
            .unwrap();
        let second = queue
            .steer_text_with_owner("second direction".into(), true, Some("mcp".into()))
            .unwrap();

        let delivered = queue.drain_steering();
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
            .steer_text_with_owner("focus".into(), true, Some("mcp".into()))
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
        let a = queue.add("first", "composer", false).unwrap();
        queue.add("second", "composer", false).unwrap();
        queue.run_next(&a.id).unwrap();

        let result = queue.take_next();
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
