//! Bounded retry budgets and typed retry decisions.
//!
//! A retry decision is derived from evidence, never assumed. The two failure
//! modes this exists to prevent are an unbounded retry loop (which on `main`
//! could spin forever once durable writes were refused) and a retry of work
//! that may already have taken effect.

use serde::{Deserialize, Serialize};

/// Hard ceiling on attempts for any single bounded unit of work.
pub const MAX_ATTEMPTS_CEILING: u32 = 8;

/// Why an automatic retry is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StandDownReason {
    /// The effect may already have landed. Retrying could duplicate it.
    DeliveryUnproven,
    /// The effect is known to have landed. A retry would be a second effect.
    AlreadyDelivered,
    /// The caller asked to cancel.
    Cancelled,
    /// The host is shutting down.
    Quiescing,
    /// The failure is not of a kind a retry can fix.
    NotTransient,
}

/// The typed answer to "may this be attempted again?".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision")]
pub enum RetryDecision {
    /// Attempt again. `attempt` is one-based and always `<= max_attempts`.
    Retry { attempt: u32, backoff_ms: u64 },
    /// Do not retry, and say why.
    StandDown { reason: StandDownReason },
    /// The budget is spent.
    Exhausted { attempts: u32 },
}

impl RetryDecision {
    pub fn is_retry(&self) -> bool {
        matches!(self, Self::Retry { .. })
    }
}

/// A bounded retry budget.
///
/// `max_attempts` is clamped to [`MAX_ATTEMPTS_CEILING`] on construction, so a
/// caller cannot configure an unbounded loop even by mistake.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryBudget {
    max_attempts: u32,
    attempts_used: u32,
    base_backoff_ms: u64,
}

impl RetryBudget {
    pub fn new(max_attempts: u32, base_backoff_ms: u64) -> Self {
        Self {
            max_attempts: max_attempts.clamp(1, MAX_ATTEMPTS_CEILING),
            attempts_used: 0,
            base_backoff_ms: base_backoff_ms.min(60_000),
        }
    }

    pub fn attempts_used(&self) -> u32 {
        self.attempts_used
    }

    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    pub fn exhausted(&self) -> bool {
        self.attempts_used >= self.max_attempts
    }

    /// Consume one attempt if the budget allows and the evidence permits.
    ///
    /// `retryable` is the caller's evidence-derived answer; passing `Err` makes
    /// the refusal explicit in the returned decision rather than silently
    /// consuming budget.
    pub fn next(&mut self, retryable: Result<(), StandDownReason>) -> RetryDecision {
        if let Err(reason) = retryable {
            return RetryDecision::StandDown { reason };
        }
        if self.exhausted() {
            return RetryDecision::Exhausted {
                attempts: self.attempts_used,
            };
        }
        self.attempts_used = self.attempts_used.saturating_add(1);
        let shift = self.attempts_used.saturating_sub(1).min(16);
        let backoff_ms = self
            .base_backoff_ms
            .saturating_mul(1u64 << shift)
            .min(60_000);
        RetryDecision::Retry {
            attempt: self.attempts_used,
            backoff_ms,
        }
    }
}

impl Default for RetryBudget {
    fn default() -> Self {
        Self::new(4, 100)
    }
}
