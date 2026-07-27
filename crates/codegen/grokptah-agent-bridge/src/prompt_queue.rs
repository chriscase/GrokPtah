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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptQueueTakeResult {
    pub batch: Option<PromptQueueBatch>,
    pub entries: Vec<PromptQueueEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptQueueRunNextResult {
    pub entries: Vec<PromptQueueEntry>,
    pub cancelled_active: bool,
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

#[derive(Debug, Default)]
pub(crate) struct SessionPromptQueue {
    queued: VecDeque<PromptQueueEntry>,
    steering: VecDeque<PromptQueueEntry>,
}

impl SessionPromptQueue {
    pub fn list(&self) -> Vec<PromptQueueEntry> {
        self.queued.iter().cloned().collect()
    }

    pub fn add(
        &mut self,
        text: impl Into<String>,
        source: impl Into<String>,
        priority: bool,
    ) -> Result<PromptQueueEntry> {
        let entry = PromptQueueEntry::new(text, source, priority)?;
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

    pub fn remove(&mut self, id: &str) -> Result<PromptQueueEntry> {
        let index = self
            .queued
            .iter()
            .position(|entry| entry.id == id)
            .ok_or_else(|| anyhow::anyhow!("unknown queued prompt {id}"))?;
        Ok(self.queued.remove(index).expect("queue index exists"))
    }

    pub fn clear(&mut self) {
        self.queued.clear();
    }

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

    pub fn steer_text(&mut self, text: String, can_inject: bool) -> Result<SteeringReceipt> {
        let source = if can_inject {
            "steer_now"
        } else {
            "steering_deferred"
        };
        let entry = PromptQueueEntry::new(text, source, !can_inject)?;
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
        self.steering.drain(..).collect()
    }

    pub fn defer_pending_steering(&mut self) -> usize {
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

    pub fn take_next(&mut self) -> PromptQueueTakeResult {
        if self.queued.is_empty() {
            return PromptQueueTakeResult {
                batch: None,
                entries: Vec::new(),
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
