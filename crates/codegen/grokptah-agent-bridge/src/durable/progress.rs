//! Action stationarity that distinguishes "stuck" from "waiting".
//!
//! `main` classifies a turn as stationary from the **tool call signature
//! alone** (`host_helpers::IdenticalToolCallRun`). A model polling a build log
//! issues a byte-identical call every round, so a run that is making real
//! progress is indistinguishable from one that is stuck, and it is stopped at
//! the identical-call ceiling with `RunStopCause::Stationarity`.
//!
//! Repetition is only stationary when the *observation* also fails to move.
//! This ledger therefore takes two inputs per round — the call signature and
//! the raw observation digest — and refuses to call a run stationary while the
//! evidence says it is advancing.
//!
//! The digest must come from [`super::observation::RawObservation`], i.e. from
//! the raw bytes before the 24,000-byte wire bound. A digest taken after the
//! bound cannot see a change beyond it, which turns a long, advancing output
//! into a false inert repeat.

use super::observation::RawObservationDigest;

/// Consecutive identical calls whose observations also never moved.
pub const MAX_INERT_REPEATS: u32 = 4;
/// Consecutive calls that are no-ops by construction.
pub const MAX_TRUE_NOOPS: u32 = 4;
/// Consecutive identical calls for which no observation evidence exists.
pub const MAX_UNOBSERVED_REPEATS: u32 = 16;
/// Nudge the model once a repeat run reaches this length.
pub const NUDGE_AFTER_REPEATS: u32 = 8;

const _: () = assert!(MAX_INERT_REPEATS < NUDGE_AFTER_REPEATS);
const _: () = assert!(MAX_TRUE_NOOPS < NUDGE_AFTER_REPEATS);
const _: () = assert!(NUDGE_AFTER_REPEATS < MAX_UNOBSERVED_REPEATS);

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
    /// Whether this class can ever justify a stationarity stop.
    ///
    /// `Advancing` cannot: a run that is producing new output is bounded by
    /// rounds, duration and tokens, not by stationarity.
    pub fn can_stop(self) -> bool {
        matches!(self, Self::TrueNoop | Self::Inert | Self::Unobserved)
    }

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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopDetail {
    pub class: RepeatClass,
    pub repeats: u32,
    pub tool_name: String,
    /// Fingerprint of the observation that never moved, for an inert repeat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_fingerprint: Option<String>,
}

/// The ledger's answer for one round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopDecision {
    /// Keep going.
    Continue,
    /// Stop the turn; `detail` says why, structurally.
    Stop(StopDetail),
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
    /// Consecutive rounds where signature *and* observation both repeated.
    inert_run_len: u32,
    /// Whether any round in this signature run produced a different observation.
    saw_advance: bool,
    /// Whether an observation was recorded for the current round.
    observed_this_round: bool,
    nudged: bool,
}

impl ProgressLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the call the model just issued. Returns the length of the
    /// current identical-signature run.
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
        self.observed_this_round = false;
        self.tool_name = tool_name.to_string();
        self.signature_run_len
    }

    /// Record the raw observation the round produced.
    ///
    /// `digest` must be taken from the raw output, before any bounded
    /// projection. Passing a digest of already-truncated text reintroduces
    /// exactly the false-inert defect this ledger exists to remove.
    pub fn observe_outcome(&mut self, digest: RawObservationDigest) {
        self.observed_this_round = true;
        match self.last_observation {
            Some(previous) if previous == digest && self.signature_run_len > 1 => {
                self.inert_run_len = self.inert_run_len.saturating_add(1);
            }
            Some(_) => {
                // The observation moved: this run is not stationary, and the
                // inert evidence collected so far no longer describes it.
                self.inert_run_len = 0;
                self.saw_advance = true;
            }
            None => {
                self.inert_run_len = 0;
            }
        }
        self.last_observation = Some(digest);
    }

    /// How the current round classifies.
    pub fn class(&self) -> RepeatClass {
        if self.signature_run_len <= 1 {
            return RepeatClass::Fresh;
        }
        if self.is_true_noop_run {
            return RepeatClass::TrueNoop;
        }
        if self.saw_advance {
            return RepeatClass::Advancing;
        }
        if self.inert_run_len > 0 {
            return RepeatClass::Inert;
        }
        if self.observed_this_round {
            // One observation recorded but no prior to compare against.
            return RepeatClass::Unobserved;
        }
        RepeatClass::Unobserved
    }

    /// Fire the one-shot nudge when a repeat run gets long, regardless of class.
    ///
    /// A nudge is advice, not a terminal decision, so it is safe on an
    /// advancing run and is what lets a genuinely stuck model recover before a
    /// ceiling is reached.
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
    pub fn decide(&self) -> StopDecision {
        let class = self.class();
        if !class.can_stop() {
            return StopDecision::Continue;
        }
        let (count, ceiling) = match class {
            RepeatClass::TrueNoop => (self.signature_run_len, MAX_TRUE_NOOPS),
            RepeatClass::Inert => (self.inert_run_len.saturating_add(1), MAX_INERT_REPEATS),
            RepeatClass::Unobserved => (self.signature_run_len, MAX_UNOBSERVED_REPEATS),
            RepeatClass::Fresh | RepeatClass::Advancing => return StopDecision::Continue,
        };
        if count < ceiling {
            return StopDecision::Continue;
        }
        StopDecision::Stop(StopDetail {
            class,
            repeats: self.signature_run_len,
            tool_name: self.tool_name.clone(),
            observation_fingerprint: if class == RepeatClass::Inert {
                self.last_observation.map(|d| d.fingerprint())
            } else {
                None
            },
        })
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
