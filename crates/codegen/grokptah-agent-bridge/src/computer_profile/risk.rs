//! Deterministic risk classification from operator policy and host observation.
//!
//! Model output is never used to lower risk. False positives only cost an
//! escalation or an abstention; false negatives could run a consequential
//! objective under a routine profile.

use serde::{Deserialize, Serialize};

use crate::computer_use::{ComputerObservation, Sensitivity};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskRisk {
    #[default]
    Routine,
    Consequential,
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

fn normalize(input: &str, elide_connectors: bool) -> String {
    let mut normalized = String::with_capacity(input.len() + 2);
    normalized.push(' ');
    let mut in_gap = true;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            in_gap = false;
        } else if ch.is_alphanumeric() {
            normalized.push(ch);
            in_gap = false;
        } else if elide_connectors && matches!(ch, '-' | '_') {
            // Also scan a separated form so "rm -rf" remains detectable.
        } else if !in_gap {
            normalized.push(' ');
            in_gap = true;
        }
    }
    if !normalized.ends_with(' ') {
        normalized.push(' ');
    }
    normalized
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    haystack.contains(&format!(" {needle} "))
}

fn matches_any(input: &str, needles: &[&str]) -> bool {
    let separated = normalize(input, false);
    let elided = normalize(input, true);
    needles
        .iter()
        .any(|needle| contains_word(&separated, needle) || contains_word(&elided, needle))
}

pub fn classify_objective(objective: &str) -> TaskRisk {
    if matches_any(objective, DESTRUCTIVE_NEEDLES) {
        TaskRisk::Destructive
    } else if matches_any(objective, CONSEQUENTIAL_NEEDLES) {
        TaskRisk::Consequential
    } else {
        TaskRisk::Routine
    }
}

pub fn classify_task(objective: &str, observation: &ComputerObservation) -> TaskRisk {
    let text_risk = classify_objective(objective);
    let hard_denied = observation.sensitivity.is_hard_denied()
        || observation.target.sensitivity.is_hard_denied()
        || observation
            .elements
            .iter()
            .any(|element| element.sensitivity.is_hard_denied());
    if hard_denied {
        return TaskRisk::Destructive;
    }
    let sensitive = observation.sensitivity == Sensitivity::Potential
        || observation.target.sensitivity == Sensitivity::Potential
        || observation
            .elements
            .iter()
            .any(|element| element.sensitivity == Sensitivity::Potential);
    if sensitive {
        text_risk.max(TaskRisk::Consequential)
    } else {
        text_risk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destructive_wins_and_word_boundaries_are_exact() {
        assert_eq!(
            classify_objective("send the report and then delete the draft"),
            TaskRisk::Destructive
        );
        assert_eq!(
            classify_objective("show the undeleted records"),
            TaskRisk::Routine
        );
        assert_eq!(classify_objective("RM -RF /tmp/out"), TaskRisk::Destructive);
        assert_eq!(
            classify_objective("un-install the application"),
            TaskRisk::Destructive
        );
    }

    #[test]
    fn consequential_objectives_are_not_model_decided() {
        assert_eq!(
            classify_objective("Submit the expense claim"),
            TaskRisk::Consequential
        );
        assert_eq!(
            classify_objective("find the row for order 41"),
            TaskRisk::Routine
        );
    }
}
