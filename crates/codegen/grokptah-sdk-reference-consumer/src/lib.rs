//! A consumer that embeds the GrokPtah harness through the public seam only.
//!
//! This crate is the standing proof for a claim that is otherwise just a
//! sentence in a design document: *an external project can drive a GrokPtah
//! agent host without being able to reach the runtime's internals.* It is
//! written the way ContextDesk would be written, and it is deliberately small,
//! because the interesting content is what it **cannot** do.
//!
//! It depends on exactly one crate. Not the bridge, not the service, not a
//! hand-rolled JSON-RPC client. Everything below is expressed in published
//! types, and `tests/isolation.rs` fails the build if that ever stops being
//! true.

#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

use grokptah_agent_sdk::prelude::*;

/// A consumer's view of one run, carrying only what it can act on.
///
/// Note what a consumer *cannot* put here even if it wanted to: there is no
/// field for the prompt, the final response, the workspace path, the bearer
/// token, or the originating request id, because no published type exposes
/// one. The restriction is structural, not a coding convention this file
/// happens to follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedRun {
    pub run_id: RunId,
    pub lifecycle: RunLifecycle,
    pub revision: Revision,
    /// `None` until the host reports one this build understands.
    pub stop_cause: Option<StopCause>,
}

impl TrackedRun {
    pub fn from_view(view: &RunView) -> Self {
        Self {
            run_id: view.run_id.clone(),
            lifecycle: view.lifecycle.clone(),
            revision: view.revision,
            stop_cause: view.stop_cause.clone(),
        }
    }

    /// Whether this consumer should keep observing.
    ///
    /// A lifecycle this build cannot read counts as live. Guessing "finished"
    /// would stop the consumer watching a run that may still be executing and
    /// let it report an outcome the host never produced.
    pub fn should_keep_observing(&self) -> bool {
        !self.lifecycle.is_terminal()
    }
}

/// Track a run across observations, refusing anything that goes backwards.
///
/// The watermark is the consumer-side half of the contract's monotonic
/// revision rule: a stale snapshot arriving after a fresher one — reordered by
/// a proxy, replayed by a retry — is dropped rather than applied.
#[derive(Debug, Default)]
pub struct RunTracker {
    watermark: RevisionWatermark,
    latest: Option<TrackedRun>,
}

impl RunTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` when the observation was fresh and was applied.
    pub fn observe(&mut self, view: &RunView) -> bool {
        if self.watermark.admit(view.revision).is_err() {
            return false;
        }
        self.latest = Some(TrackedRun::from_view(view));
        true
    }

    pub fn latest(&self) -> Option<&TrackedRun> {
        self.latest.as_ref()
    }
}

/// What a consumer may safely do after a failed mutation.
///
/// Three-valued on purpose. Collapsing `Unsafe` into "do not retry" would lose
/// the one case that matters: an uncertain outcome may already have applied,
/// so an automatic retry can double-apply real work.
pub fn recovery_advice(error: &SdkError) -> RetryDisposition {
    error.code.retry_disposition()
}

/// Summarize a receipt page without ever asserting that absence is proof.
///
/// A consumer reading "zero receipts" and concluding "no mutation happened"
/// would be wrong twice over: the window is a host-wide budget another run's
/// traffic can consume, and receipts of live runs are exempt from it. The
/// summary therefore always carries the window it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptSummary {
    pub settled: usize,
    pub uncertain: usize,
    pub window: ReceiptRetention,
    pub more_available: bool,
}

pub fn summarize(page: &ReceiptPage) -> ReceiptSummary {
    ReceiptSummary {
        settled: page.items.iter().filter(|r| r.is_settled()).count(),
        uncertain: page.items.iter().filter(|r| r.is_uncertain()).count(),
        window: page.retention.clone(),
        more_available: !page.is_caught_up(),
    }
}
