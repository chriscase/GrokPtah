//! The run engine port, and a deterministic offline engine.
//!
//! The host owns lifecycle, durability, authority, and projection. It does not
//! own model execution: that arrives through [`RunEngine`]. Keeping the port
//! narrow is what lets this crate be exercised end to end with no provider
//! credential, no network, and no live model.
//!
//! [`FixtureEngine`] is the offline implementation that ships with the host. It
//! replays a scripted, byte-for-byte deterministic outcome sequence, so restart
//! recovery, escalation, and receipt tests observe the same run every time.

use std::collections::BTreeMap;
use std::path::Path;

use grokptah_agent_sdk::RunScope;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::attention::AttentionKind;
use crate::error::{HostError, HostResult, io_error};
use crate::identity::ExternalRef;
use crate::lifecycle::CancelSignal;

/// One file an engine reports as changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineChangedFile {
    /// Repository-relative path.
    pub path: String,
    /// Bounded human-readable summary.
    pub summary: String,
}

/// What one engine step produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EngineOutcome {
    /// The run advanced and produced a bounded update.
    Progress {
        /// Structured update, redacted by the host before it is journaled.
        update: Value,
    },
    /// The run stopped and needs an operator decision.
    ///
    /// The field is named `attention` rather than `kind` because `kind` is the
    /// enum's own tag on the wire.
    NeedsAttention {
        /// Why the run stopped.
        attention: AttentionKind,
        /// Stable machine-readable reason.
        reason_code: String,
        /// Operator-facing detail, redacted by the host.
        detail: String,
    },
    /// The run finished successfully and produced reviewable changes.
    Completed {
        /// Files the run changed. May be empty for a no-op run.
        #[serde(default)]
        changed_files: Vec<EngineChangedFile>,
        /// Bounded unified diff.
        #[serde(default)]
        diff: String,
        /// Final workspace fingerprint recorded by the engine.
        fingerprint: String,
    },
    /// The run failed terminally.
    Failed {
        /// Stable machine-readable reason.
        reason_code: String,
        /// Operator-facing detail, redacted by the host.
        detail: String,
    },
}

/// One step the host asks the engine to take.
#[derive(Debug, Clone, Copy)]
pub struct EngineStep<'a> {
    /// Exact run identity.
    pub scope: &'a RunScope,
    /// One-based round number within the run's bounds.
    pub round: u16,
    /// The admitted prompt.
    pub prompt: &'a str,
    /// Steering directives accepted since the last step, oldest first.
    pub steering: &'a [String],
    /// Cooperative cancellation for this step. An engine that may block should
    /// check it at each checkpoint; one that cannot is simply never cancelled.
    pub cancel: &'a CancelSignal,
    /// One-based dispatch ordinal, unique per run.
    ///
    /// The host records this before the step runs, so an engine that dispatches
    /// externally can bind its own idempotency to the exact attempt the host
    /// will find on disk if this process dies mid-step.
    pub dispatch_ordinal: u32,
}

/// What a step established about work that may have left this host.
///
/// This is a host-side projection, not a second delivery state machine. The
/// component that actually talks to a provider owns the full vocabulary — what
/// was bound, what was presented, how far the answer got. All the host needs
/// from it is the one question it must answer on its own: *may this run move?*
///
/// The safe default is [`Indeterminate`](Self::Indeterminate). Anything an
/// engine cannot prove belongs there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchDisposition {
    /// Nothing left this host. There is nothing to reconcile.
    Local,
    /// An external dispatch was prepared and provably did not leave.
    NotDispatched,
    /// An external dispatch happened and settled durably.
    Resolved,
    /// Whether an external dispatch happened cannot be established.
    Indeterminate,
}

impl DispatchDisposition {
    /// Whether the host may advance the run on this result.
    ///
    /// An indeterminate dispatch is the one case where continuing could repeat
    /// work that already happened, so the run halts and waits for a human.
    pub fn may_advance(self) -> bool {
        !matches!(self, Self::Indeterminate)
    }

    /// Stable label for records and events.
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::NotDispatched => "not_dispatched",
            Self::Resolved => "resolved",
            Self::Indeterminate => "indeterminate",
        }
    }
}

/// What one step established about external dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DispatchReport {
    /// Whether the run may advance.
    pub disposition: DispatchDisposition,
    /// Reference to the attempt record the orchestrator wrote, when there is
    /// one. Opaque here: the orchestrator's contract owns its meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<ExternalRef>,
    /// Reference to the operation receipt the orchestrator wrote, when there is
    /// one. Opaque here for the same reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ExternalRef>,
}

impl DispatchReport {
    /// A step that dispatched nothing outside this host.
    pub fn local() -> Self {
        Self {
            disposition: DispatchDisposition::Local,
            attempt: None,
            receipt: None,
        }
    }

    /// A step that reports an external disposition and its references.
    pub fn external(
        disposition: DispatchDisposition,
        attempt: Option<ExternalRef>,
        receipt: Option<ExternalRef>,
    ) -> Self {
        Self {
            disposition,
            attempt,
            receipt,
        }
    }

    /// Whether every carried reference is still within bounds.
    pub fn refs_are_bounded(&self) -> bool {
        self.attempt.as_ref().is_none_or(ExternalRef::is_bounded)
            && self.receipt.as_ref().is_none_or(ExternalRef::is_bounded)
    }
}

/// The full result of one engine step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StepResult {
    /// What the run produced.
    pub outcome: EngineOutcome,
    /// What the step established about external dispatch.
    pub dispatch: DispatchReport,
}

impl StepResult {
    /// A result from an engine that never dispatches outside this host.
    pub fn local(outcome: EngineOutcome) -> Self {
        Self {
            outcome,
            dispatch: DispatchReport::local(),
        }
    }
}

/// The seam between host lifecycle and model execution.
pub trait RunEngine: Send {
    /// Stable label reported by health.
    fn label(&self) -> &'static str;

    /// Advance one run by one bounded step.
    fn step(&mut self, step: &EngineStep<'_>) -> StepResult;
}

/// Scripted outcomes for the offline engine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FixtureScript {
    /// Outcome sequences keyed by exact prompt.
    #[serde(default)]
    pub prompts: BTreeMap<String, Vec<EngineOutcome>>,
    /// Sequence used when no prompt matches.
    #[serde(default)]
    pub default: Vec<EngineOutcome>,
}

impl FixtureScript {
    /// Read and validate a fixture script.
    pub fn load(path: &Path) -> HostResult<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|error| io_error("fixture_unreadable", &error))?;
        Self::parse(&raw)
    }

    /// Parse a fixture script from its serialized form.
    pub fn parse(raw: &str) -> HostResult<Self> {
        serde_json::from_str(raw).map_err(|_| {
            HostError::invalid("fixture_malformed", "fixture script is not a valid script")
        })
    }
}

/// Deterministic, offline engine driven by a [`FixtureScript`].
#[derive(Debug)]
pub struct FixtureEngine {
    script: FixtureScript,
    cursors: BTreeMap<String, usize>,
}

impl FixtureEngine {
    /// Build an engine over a script.
    pub fn new(script: FixtureScript) -> Self {
        Self {
            script,
            cursors: BTreeMap::new(),
        }
    }

    fn sequence(&self, prompt: &str) -> &[EngineOutcome] {
        self.script
            .prompts
            .get(prompt)
            .map_or(self.script.default.as_slice(), Vec::as_slice)
    }
}

impl RunEngine for FixtureEngine {
    fn label(&self) -> &'static str {
        "fixture"
    }

    fn step(&mut self, step: &EngineStep<'_>) -> StepResult {
        let length = self.sequence(step.prompt).len();
        if length == 0 {
            return StepResult::local(EngineOutcome::Failed {
                reason_code: "fixture_missing".to_owned(),
                detail: "no scripted outcome for this prompt".to_owned(),
            });
        }
        let cursor = self.cursors.entry(step.scope.run_id.clone()).or_insert(0);
        let index = (*cursor).min(length - 1);
        *cursor = index + 1;
        // The fixture engine reaches nothing outside this host, so every step
        // is `Local`: there is never an attempt to reconcile.
        StepResult::local(self.sequence(step.prompt)[index].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope() -> RunScope {
        RunScope {
            session_id: "session-1".into(),
            workspace: "/approved".into(),
            run_id: "run-1".into(),
        }
    }

    fn script() -> FixtureScript {
        FixtureScript::parse(
            r#"{
              "prompts": {
                "build": [
                  {"kind": "progress", "update": {"note": "planning"}},
                  {"kind": "completed", "changedFiles": [{"path": "src/a.rs", "summary": "edit"}],
                   "diff": "--- a\n+++ b\n", "fingerprint": "fp-1"}
                ]
              },
              "default": [{"kind": "failed", "reasonCode": "unscripted", "detail": "no script"}]
            }"#,
        )
        .expect("script parses")
    }

    fn take(engine: &mut FixtureEngine, scope: &RunScope, round: u16, prompt: &str) -> StepResult {
        let cancel = CancelSignal::new();
        engine.step(&EngineStep {
            scope,
            round,
            prompt,
            steering: &[],
            cancel: &cancel,
            dispatch_ordinal: u32::from(round),
        })
    }

    #[test]
    fn a_scripted_prompt_replays_the_same_sequence_every_time() {
        let run = |engine: &mut FixtureEngine| {
            let scope = scope();
            (1..=2)
                .map(|round| take(engine, &scope, round, "build"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            run(&mut FixtureEngine::new(script())),
            run(&mut FixtureEngine::new(script()))
        );
    }

    #[test]
    fn the_sequence_advances_then_holds_its_terminal_outcome() {
        let mut engine = FixtureEngine::new(script());
        let scope = scope();
        assert_eq!(
            take(&mut engine, &scope, 1, "build").outcome,
            EngineOutcome::Progress {
                update: json!({"note": "planning"})
            }
        );
        for round in 2..=4 {
            let result = take(&mut engine, &scope, round, "build");
            assert!(matches!(result.outcome, EngineOutcome::Completed { .. }));
        }
    }

    #[test]
    fn a_local_engine_never_reports_anything_to_reconcile() {
        let mut engine = FixtureEngine::new(script());
        let result = take(&mut engine, &scope(), 1, "build");
        assert_eq!(result.dispatch, DispatchReport::local());
        assert_eq!(result.dispatch.disposition, DispatchDisposition::Local);
        assert!(result.dispatch.disposition.may_advance());
        assert!(result.dispatch.refs_are_bounded());
        assert!(!DispatchDisposition::Indeterminate.may_advance());
        assert_eq!(DispatchDisposition::NotDispatched.label(), "not_dispatched");
    }

    #[test]
    fn an_unscripted_prompt_falls_back_and_an_empty_script_fails_closed() {
        let scope = scope();
        let mut engine = FixtureEngine::new(script());
        assert_eq!(
            take(&mut engine, &scope, 1, "unknown").outcome,
            EngineOutcome::Failed {
                reason_code: "unscripted".to_owned(),
                detail: "no script".to_owned(),
            }
        );

        let mut empty = FixtureEngine::new(FixtureScript::default());
        let result = take(&mut empty, &scope, 1, "anything");
        assert!(matches!(
            result.outcome,
            EngineOutcome::Failed { reason_code, .. } if reason_code == "fixture_missing"
        ));
        assert_eq!(result.dispatch.disposition, DispatchDisposition::Local);
        assert_eq!(empty.label(), "fixture");
    }

    #[test]
    fn a_malformed_script_is_refused() {
        assert_eq!(
            FixtureScript::parse("{\"unexpected\": 1}")
                .expect_err("unknown keys are refused")
                .reason_code(),
            "fixture_malformed"
        );
    }
}
