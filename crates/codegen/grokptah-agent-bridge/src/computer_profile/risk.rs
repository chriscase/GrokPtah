//! Deterministic task-risk classification for adaptive profile selection.
//!
//! Risk is derived from two things the host already owns: the local operator's
//! objective text, and the sensitivity the observation reports. It is never
//! derived from the model's own account of what it is about to do — a model
//! that wants to delete something has every incentive to describe it as tidying
//! up, so its prose is input to nothing here.
//!
//! Classification is lexical, bounded, and total. It runs on a normalized copy
//! of the objective, never allocates per-needle, and returns the same class for
//! the same bytes every time. It is deliberately **not** a safety control: a
//! destructive objective still passes through every kernel check unchanged. It
//! only decides how much assurance the run must buy before it starts, which is
//! why a false positive costs an escalation and never costs an action.

use serde::{Deserialize, Serialize};

use crate::computer_use::{ComputerObservation, Sensitivity};

/// How consequential the local operator's objective is.
///
/// Ordering is severity order, so `risk >= TaskRisk::Consequential` reads
/// correctly.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskRisk {
    /// Reading, navigating, filling a field, or any other reversible move.
    #[default]
    Routine,
    /// Visible to someone else or hard to walk back: sending, publishing,
    /// paying, installing, approving.
    Consequential,
    /// Destroys state: deleting, wiping, formatting, revoking, resetting.
    Destructive,
}

impl TaskRisk {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Routine => "routine",
            Self::Consequential => "consequential",
            Self::Destructive => "destructive",
        }
    }
}

/// Verbs whose ordinary meaning is "state stops existing".
const DESTRUCTIVE_NEEDLES: &[&str] = &[
    "delete",
    "deletes",
    "deleting",
    "erase",
    "erases",
    "erasing",
    "wipe",
    "wipes",
    "wiping",
    "destroy",
    "destroys",
    "destroying",
    "format",
    "formats",
    "formatting",
    "uninstall",
    "uninstalls",
    "uninstalling",
    "purge",
    "purges",
    "purging",
    "remove all",
    "delete all",
    "drop table",
    "drop database",
    "truncate",
    "rm rf",
    "factory reset",
    "reset to factory",
    "revoke",
    "revokes",
    "revoking",
    "deactivate",
    "deactivates",
    "deactivating",
    "terminate account",
    "close account",
    "delete account",
    "empty trash",
    "shred",
    "overwrite",
    "overwrites",
    "overwriting",
];

/// Verbs whose ordinary meaning is "someone else sees this" or "money moves".
const CONSEQUENTIAL_NEEDLES: &[&str] = &[
    "send",
    "sends",
    "sending",
    "submit",
    "submits",
    "submitting",
    "publish",
    "publishes",
    "publishing",
    "post",
    "posts",
    "posting",
    "share",
    "shares",
    "sharing",
    "pay",
    "pays",
    "paying",
    "payment",
    "purchase",
    "purchases",
    "purchasing",
    "buy",
    "buys",
    "buying",
    "checkout",
    "transfer",
    "transfers",
    "transferring",
    "wire funds",
    "email",
    "emails",
    "emailing",
    "install",
    "installs",
    "installing",
    "approve",
    "approves",
    "approving",
    "authorize",
    "authorizes",
    "authorizing",
    "sign",
    "signs",
    "signing",
    "confirm",
    "confirms",
    "confirming",
    "deploy",
    "deploys",
    "deploying",
    "merge",
    "merges",
    "merging",
    "invite",
    "invites",
    "inviting",
    "subscribe",
    "subscribes",
    "subscribing",
    "place order",
    "place the order",
];

/// Longest needle phrase, in words. Asserted in the tests so a future needle
/// cannot quietly turn matching into an unbounded phrase scan.
#[cfg(test)]
const MAX_NEEDLE_WORDS: usize = 3;

/// How intra-word connectors (`-` and `_`) are treated during normalization.
///
/// Both readings are needed and they disagree, which is why classification
/// scans both rather than picking one:
///
/// - `rm -rf` is two tokens joined by punctuation, so the hyphen must become a
///   separator or the phrase needle never matches.
/// - `un-install` is one word interrupted by a hyphen, so the hyphen must
///   vanish or the verb never matches.
///
/// Scanning both forms is deterministic, costs two passes, and cannot invent a
/// match that neither reading supports: needles are whole-word, so eliding a
/// hyphen inside `un-deleted` yields `undeleted`, which still does not contain
/// the word `delete`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Connector {
    /// `un-install` -> `un install`
    Separate,
    /// `un-install` -> `uninstall`
    Elide,
}

/// Normalizes an objective for lexical matching.
///
/// ASCII-lowercases, maps every run of non-alphanumeric bytes to a single space
/// (except intra-word connectors under [`Connector::Elide`]), and pads with
/// spaces so needle matching is word-boundary exact.
fn normalize(objective: &str, connector: Connector) -> String {
    let mut out = String::with_capacity(objective.len() + 2);
    out.push(' ');
    let mut in_gap = true;
    for ch in objective.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            in_gap = false;
        } else if ch.is_alphanumeric() {
            // Keep non-ASCII word characters verbatim so a non-English
            // objective is not silently shredded into separators.
            out.push(ch);
            in_gap = false;
        } else if connector == Connector::Elide && (ch == '-' || ch == '_') {
            // Drop it without opening a gap, joining the two halves.
        } else if !in_gap {
            out.push(' ');
            in_gap = true;
        }
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

/// True when any needle matches either normalized reading of the objective.
fn matches_any(objective: &str, needles: &[&str]) -> bool {
    let separated = normalize(objective, Connector::Separate);
    let elided = normalize(objective, Connector::Elide);
    needles
        .iter()
        .any(|needle| contains_word(&separated, needle) || contains_word(&elided, needle))
}

/// True when `haystack` (already normalized and space-padded) contains
/// `needle` as a whole word or whole phrase.
fn contains_word(haystack: &str, needle: &str) -> bool {
    debug_assert!(needle.chars().all(|c| c.is_ascii_lowercase() || c == ' '));
    let mut padded = String::with_capacity(needle.len() + 2);
    padded.push(' ');
    padded.push_str(needle);
    padded.push(' ');
    haystack.contains(&padded)
}

/// Classifies the operator's objective text alone.
///
/// Destructive is checked before consequential so "delete and send the report"
/// classifies as destructive rather than as whichever needle happened to appear
/// first in the string.
pub fn classify_objective(objective: &str) -> TaskRisk {
    if matches_any(objective, DESTRUCTIVE_NEEDLES) {
        return TaskRisk::Destructive;
    }
    if matches_any(objective, CONSEQUENTIAL_NEEDLES) {
        return TaskRisk::Consequential;
    }
    TaskRisk::Routine
}

/// Classifies the whole task: the objective plus what the surface reports.
///
/// A surface the host has marked `Potential`ly sensitive raises a routine task
/// to consequential even when the objective reads innocently, because the risk
/// of acting on a credential-adjacent surface does not depend on how the
/// operator phrased the request. Hard-denied surfaces are not represented here
/// at all: the kernel refuses those outright, before any profile exists.
pub fn classify_task(objective: &str, observation: &ComputerObservation) -> TaskRisk {
    let from_text = classify_objective(objective);
    let sensitive = observation.sensitivity == Sensitivity::Potential
        || observation.target.sensitivity == Sensitivity::Potential
        || observation
            .elements
            .iter()
            .any(|element| element.sensitivity == Sensitivity::Potential);
    if sensitive {
        from_text.max(TaskRisk::Consequential)
    } else {
        from_text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destructive_verbs_classify_destructive() {
        for objective in [
            "Delete the selected invoice",
            "please WIPE the staging database",
            "run rm -rf ./build",
            "Factory reset the device",
            "revoke Ada's access token",
            "drop table customers",
        ] {
            assert_eq!(
                classify_objective(objective),
                TaskRisk::Destructive,
                "{objective:?} should be destructive"
            );
        }
    }

    #[test]
    fn consequential_verbs_classify_consequential() {
        for objective in [
            "Send the quarterly report",
            "Submit the expense claim",
            "publish the draft post",
            "Approve the pending request",
        ] {
            assert_eq!(
                classify_objective(objective),
                TaskRisk::Consequential,
                "{objective:?} should be consequential"
            );
        }
    }

    #[test]
    fn ordinary_objectives_stay_routine() {
        for objective in [
            "Open the settings pane",
            "Type Ada Lovelace into the name field",
            "Scroll to the bottom of the list",
            "Find the row for order 41",
        ] {
            assert_eq!(
                classify_objective(objective),
                TaskRisk::Routine,
                "{objective:?} should be routine"
            );
        }
    }

    #[test]
    fn destructive_wins_over_consequential_regardless_of_word_order() {
        assert_eq!(
            classify_objective("send the summary and then delete the draft"),
            TaskRisk::Destructive
        );
        assert_eq!(
            classify_objective("delete the draft and then send the summary"),
            TaskRisk::Destructive
        );
    }

    #[test]
    fn punctuation_and_casing_do_not_hide_intent() {
        assert_eq!(classify_objective("RM -RF /tmp/x"), TaskRisk::Destructive);
        assert_eq!(
            classify_objective("un-install the app"),
            TaskRisk::Destructive
        );
        assert_eq!(classify_objective("...DELETE!!!"), TaskRisk::Destructive);
    }

    #[test]
    fn substrings_do_not_produce_false_positives() {
        // "undeleted" contains "delete"; whole-word matching must not fire.
        assert_eq!(
            classify_objective("show the undeleted records"),
            TaskRisk::Routine
        );
        // "signature" contains "sign".
        assert_eq!(
            classify_objective("read the signature block"),
            TaskRisk::Routine
        );
        // "reorder" contains "order", and a bare "order" is a noun far more
        // often than a verb, so it is not a needle at all.
        assert_eq!(classify_objective("reorder the columns"), TaskRisk::Routine);
        assert_eq!(
            classify_objective("open the sales order in order to check it"),
            TaskRisk::Routine
        );
        // Eliding a hyphen must not manufacture a verb either.
        assert_eq!(
            classify_objective("list the un-deleted rows"),
            TaskRisk::Routine
        );
        assert_eq!(
            classify_objective("check the re-order point"),
            TaskRisk::Routine
        );
    }

    #[test]
    fn both_hyphen_readings_are_scanned() {
        // Joined reading: the hyphen must vanish for the verb to appear.
        assert_eq!(
            classify_objective("un-install the app"),
            TaskRisk::Destructive
        );
        assert_eq!(
            classify_objective("un_install the app"),
            TaskRisk::Destructive
        );
        // Separated reading: the hyphen must become a gap for the phrase to
        // appear. These two requirements are why both forms are scanned.
        assert_eq!(classify_objective("rm -rf ./out"), TaskRisk::Destructive);
        assert_eq!(
            classify_objective("do a factory-reset"),
            TaskRisk::Destructive
        );
    }

    #[test]
    fn classification_is_deterministic_and_total() {
        let inputs = ["", "   ", "\u{0}", "\u{1F600}", &"a".repeat(4096)];
        for input in inputs {
            let first = classify_objective(input);
            assert_eq!(first, classify_objective(input));
            assert_eq!(first, TaskRisk::Routine);
        }
    }

    /// The classifier is allowed to over-fire on a verb used as a noun, and
    /// deliberately does. A false positive costs one escalation and never costs
    /// an action, while a false negative would run consequential work under a
    /// routine floor. `order` is the one word removed from the needle set for
    /// the opposite reason: it is common enough as a noun ("the row for order
    /// 41", "in order to") that firing on it would classify nearly everything
    /// as consequential and destroy the signal.
    #[test]
    fn over_firing_is_the_safe_direction_and_order_is_the_exception() {
        assert_eq!(
            classify_objective("open the purchase order"),
            TaskRisk::Consequential,
            "over-firing on a noun costs an escalation, not an action"
        );
        assert_eq!(
            classify_objective("find the row for order 41"),
            TaskRisk::Routine,
            "`order` is too common a noun to be a risk signal"
        );
    }

    #[test]
    fn needle_phrases_stay_within_the_documented_word_bound() {
        for needle in DESTRUCTIVE_NEEDLES.iter().chain(CONSEQUENTIAL_NEEDLES) {
            assert!(
                needle.split(' ').count() <= MAX_NEEDLE_WORDS,
                "{needle:?} exceeds the phrase bound"
            );
            assert!(
                needle.chars().all(|c| c.is_ascii_lowercase() || c == ' '),
                "{needle:?} is not normalized"
            );
        }
    }
}
