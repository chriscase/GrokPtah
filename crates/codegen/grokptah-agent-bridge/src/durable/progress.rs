//! Action stationarity that distinguishes "stuck" from "waiting".
//!
//! `main` classifies a turn as stationary from the **tool call signature
//! alone** (`host_helpers::IdenticalToolCallRun`). A model polling a build log
//! issues a byte-identical call every round, so a run that is making real
//! progress is indistinguishable from one that is stuck, and it is stopped with
//! `RunStopCause::Stationarity`.
//!
//! Two independent pieces of evidence fix that, and neither is sufficient alone:
//!
//! 1. **The observation.** A repeat is inert only when the raw observation also
//!    fails to move. The digest must come from
//!    [`super::observation::RawObservation`], i.e. from the raw bytes *before*
//!    the 24,000-byte wire bound — a digest taken after the bound cannot see a
//!    change beyond it, which turns a long, advancing output into a false inert
//!    repeat.
//! 2. **A host-issued wait witness.** Some waits legitimately return the same
//!    bytes for a long time. The host, and only the host, can say whether a
//!    poll named real, authorized, outstanding work; the model cannot asserted
//!    a wait into existence by naming a task id. See [`ActiveTaskWaitWitness`].
//!
//! The exemption is bounded: a witnessed wait is exempt from the *inert*
//! ceiling only. The identical-call ceiling, the nudge, and the run's own round
//! and duration budgets all still apply, so the wait stays bounded by authority
//! that was already there.

use uuid::Uuid;

use super::observation::RawObservationDigest;

/// Consecutive calls that are no-ops by construction. Nothing can change.
pub const MAX_TRUE_NOOPS: u32 = 4;
/// Nudge the model once a repeat run reaches this length.
pub const NUDGE_AFTER_REPEATS: u32 = 8;
/// Consecutive identical *observations* before a repeat is called inert.
///
/// Above [`NUDGE_AFTER_REPEATS`] so the one-shot nudge fires first and the
/// model gets two rounds to change approach.
pub const MAX_INERT_REPEATS: u32 = 10;
/// Consecutive identical calls with no evidence of advancement. This is the
/// outer bound `main` already applied, kept unchanged.
pub const MAX_IDENTICAL_CALLS: u32 = 16;
/// Longest a single wait is witnessed before the ordinary gates resume.
///
/// Bounded so an abandoned task cannot confer an unlimited exemption.
pub const WITNESSED_WAIT_DEADLINE_MS: u64 = 10 * 60 * 1000;

const _: () = assert!(MAX_TRUE_NOOPS < NUDGE_AFTER_REPEATS);
const _: () = assert!(NUDGE_AFTER_REPEATS < MAX_INERT_REPEATS);
const _: () = assert!(MAX_INERT_REPEATS < MAX_IDENTICAL_CALLS);

/// What the evidence says about a run of repeated calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatClass {
    /// The call signature changed; this is the first round of a new run.
    Fresh,
    /// The call is a no-op by construction, whatever it returns.
    TrueNoop,
    /// Same call, same observation. Nothing is moving.
    Inert,
    /// Same call, different observation. The run is progressing.
    Advancing,
    /// Same call, and no observation was recorded to judge it by.
    Unobserved,
}

impl RepeatClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::TrueNoop => "true_noop",
            Self::Inert => "inert",
            Self::Advancing => "advancing",
            Self::Unobserved => "unobserved",
        }
    }
}

/// Structured stationarity detail, recorded beside the terminal cause rather
/// than encoded in operator prose.
///
/// Carries no digest and no fingerprint. The observation digest exists only to
/// compare two rounds in the same turn; putting any part of it in a durable
/// record would make the record a confirmation oracle for tool output.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopDetail {
    pub class: RepeatClass,
    pub repeats: u32,
    pub tool_name: String,
}

/// The ledger's answer for one round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopDecision {
    Continue,
    Stop(StopDetail),
}

/// Typed active state of a task the host is willing to witness.
///
/// Only states in which work is genuinely outstanding. Completed, failed and
/// cancelled tasks are absent by construction: there is nothing left to wait
/// for, so a poll against one is an ordinary repeated call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveWaitState {
    Queued,
    Running,
}

impl ActiveWaitState {
    /// Map a host task status onto an active state, or `None` when the task is
    /// not outstanding. Exact matches only — no prefixes, no case folding.
    pub fn from_status(status: &str) -> Option<Self> {
        match status {
            "running" => Some(Self::Running),
            "accepted" | "proposed" | "queued" => Some(Self::Queued),
            _ => None,
        }
    }
}

/// Host-issued evidence that a wait call named real, authorized, outstanding
/// work.
///
/// Issued by the dispatcher, which is the only place that can see the task
/// registry. The model supplies an id; every field here is the *host's* answer
/// about that id, so a wait cannot be asserted into existence by naming one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveTaskWaitWitness {
    /// The exact id the host authorized, not the id the model asked for.
    pub task_id: String,
    pub state: ActiveWaitState,
    /// Session that owns the task. A poll against another session's work is not
    /// this run's wait.
    pub owner_session: Uuid,
    /// Host-assigned generation for this id. A recycled id gets a new
    /// generation, so a witness cannot be carried across identities.
    pub generation: u64,
    /// Absolute deadline in turn-relative milliseconds, past which the wait is
    /// no longer witnessed and the ordinary gates resume.
    pub deadline_ms: u64,
}

/// Whether a tool call is *shaped* like a wait.
///
/// Not sufficient for the exemption on its own: the host still has to witness
/// an authorized, active task behind it.
pub fn is_wait_shaped_tool(tool_name: &str) -> bool {
    matches!(tool_name, "task_output" | "get_task_output")
}

/// Whether a round earns the wait exemption.
///
/// Every call in the round must be wait-shaped *and* carry a witness, every
/// witness must belong to the current session, and none may be past its
/// deadline. A round mixing a poll with real work, naming an unknown or
/// finished task, or reaching for another session's task fails these and is
/// treated as ordinary work.
pub fn round_is_witnessed_wait(
    tool_names: &[&str],
    witnesses: &[ActiveTaskWaitWitness],
    session_id: Uuid,
    elapsed_ms: u64,
) -> bool {
    !tool_names.is_empty()
        && witnesses.len() == tool_names.len()
        && tool_names.iter().all(|name| is_wait_shaped_tool(name))
        && witnesses
            .iter()
            .all(|w| w.owner_session == session_id && elapsed_ms < w.deadline_ms)
}

/// Tracks whether a turn is actually making progress.
#[derive(Debug, Default)]
pub struct ProgressLedger {
    signature: Option<RawObservationDigest>,
    tool_name: String,
    signature_run_len: u32,
    is_true_noop_run: bool,
    /// Digest of the previous round's observation within this signature run.
    last_observation: Option<RawObservationDigest>,
    /// Length, in observations, of the current *unchanged suffix*.
    ///
    /// Reset to 1 whenever an observation differs from its predecessor, so a
    /// run that moved once and then froze still reaches the inert ceiling.
    inert_run_len: u32,
    /// Whether any observation in this signature run differed from the one
    /// before it. Gates only the outer identical-call ceiling, never the inert
    /// one — that distinction is what keeps a frozen suffix stoppable.
    saw_advance: bool,
    nudged: bool,
}

impl ProgressLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the call the model just issued. Returns the length of the current
    /// identical-signature run.
    ///
    /// A call that is a no-op by construction collapses to one signature, so
    /// `true` with different arguments still counts as the same run — the same
    /// rule `main` applies.
    pub fn observe_call(&mut self, signature: &str, tool_name: &str, is_true_noop: bool) -> u32 {
        let canonical = if is_true_noop {
            "\u{0}true_noop"
        } else {
            signature
        };
        let hashed = RawObservationDigest::of_raw(canonical.as_bytes());
        if self.signature == Some(hashed) {
            self.signature_run_len = self.signature_run_len.saturating_add(1);
        } else {
            self.signature = Some(hashed);
            self.signature_run_len = 1;
            self.is_true_noop_run = is_true_noop;
            self.last_observation = None;
            self.inert_run_len = 0;
            self.saw_advance = false;
            self.nudged = false;
        }
        self.tool_name = tool_name.to_string();
        self.signature_run_len
    }

    /// Record the raw observation the round produced.
    ///
    /// `digest` must be taken from the raw output, before any bounded
    /// projection. Passing a digest of already-truncated text reintroduces
    /// exactly the false-inert defect this ledger exists to remove.
    pub fn observe_outcome(&mut self, digest: RawObservationDigest) {
        if self.last_observation == Some(digest) {
            self.inert_run_len = self.inert_run_len.saturating_add(1);
        } else {
            if self.last_observation.is_some() {
                self.saw_advance = true;
            }
            // Something outside the model moved: the unchanged suffix restarts
            // at this observation rather than being forgiven entirely.
            self.inert_run_len = 1;
        }
        self.last_observation = Some(digest);
    }

    /// Exempt this round from the inert ceiling.
    ///
    /// A witnessed wait that returns the same thing is not stuck; whether it
    /// should end is for the deadline that owns it to decide. The identical-call
    /// ceiling, the nudge, and the round and duration budgets all still apply.
    pub fn observe_witnessed_wait(&mut self) {
        self.inert_run_len = 0;
        self.last_observation = None;
    }

    /// How the current round classifies.
    pub fn class(&self) -> RepeatClass {
        if self.signature_run_len <= 1 {
            return RepeatClass::Fresh;
        }
        if self.is_true_noop_run {
            return RepeatClass::TrueNoop;
        }
        if self.inert_run_len >= 2 {
            return RepeatClass::Inert;
        }
        if self.saw_advance {
            return RepeatClass::Advancing;
        }
        RepeatClass::Unobserved
    }

    /// Fire the one-shot nudge when a repeat run gets long, regardless of class.
    ///
    /// A nudge is advice, not a terminal decision, so it is safe on an advancing
    /// or witnessed run and is what lets a genuinely stuck model recover before
    /// a ceiling is reached.
    pub fn take_nudge(&mut self) -> bool {
        let fire = self.signature_run_len >= NUDGE_AFTER_REPEATS && !self.nudged;
        self.nudged |= fire;
        fire
    }

    pub fn repeats(&self) -> u32 {
        self.signature_run_len
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    /// Whether the run should stop, and the structured reason.
    ///
    /// Every applicable ceiling is checked, tightest first, so an exemption from
    /// one never removes the others.
    pub fn decide(&self) -> StopDecision {
        let stop = |class, repeats| {
            StopDecision::Stop(StopDetail {
                class,
                repeats,
                tool_name: self.tool_name.clone(),
            })
        };
        if self.signature_run_len <= 1 {
            return StopDecision::Continue;
        }
        if self.is_true_noop_run {
            return if self.signature_run_len >= MAX_TRUE_NOOPS {
                stop(RepeatClass::TrueNoop, self.signature_run_len)
            } else {
                StopDecision::Continue
            };
        }
        if self.inert_run_len >= MAX_INERT_REPEATS {
            return stop(RepeatClass::Inert, self.inert_run_len);
        }
        // The outer bound `main` already applied. It is lifted only by evidence
        // that the observations are actually moving; a witnessed wait is exempt
        // from the inert ceiling above, never from this one.
        if !self.saw_advance && self.signature_run_len >= MAX_IDENTICAL_CALLS {
            return stop(RepeatClass::Unobserved, self.signature_run_len);
        }
        StopDecision::Continue
    }
}

/// Operator-facing stop message. Host-authored template text only; no model
/// prose, no observation content.
pub fn stop_message(detail: &StopDetail) -> String {
    let reason = match detail.class {
        RepeatClass::TrueNoop => "no-op tool calls",
        RepeatClass::Inert => "identical tool calls that returned an unchanged result",
        RepeatClass::Unobserved => "identical tool calls",
        RepeatClass::Fresh | RepeatClass::Advancing => "repeated tool calls",
    };
    format!(
        "Stopped after {} consecutive {reason} (`{}`) without making progress. \
         Ask me to continue with a different approach.",
        detail.repeats, detail.tool_name
    )
}

/// One-shot nudge text.
pub fn nudge_message(tool_name: &str, repeats: u32) -> String {
    format!(
        "You have called `{tool_name}` with the same action signature {repeats} times in a row. \
         You appear to be stuck. Stop repeating it; use a different approach, wait once for \
         a long-running operation, or tell the user what is blocking progress."
    )
}
