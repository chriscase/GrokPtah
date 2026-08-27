//! Vocabulary drift.
//!
//! `schema.rs` claims its types mirror the production computer-use vocabulary
//! in `grokptah-agent-bridge`. That crate lives in a nested workspace the root
//! workspace excludes, and the benchmark deliberately does not depend on it --
//! a qualification harness that imports the implementation it is judging is
//! judging the implementation against itself.
//!
//! So the check is textual, not structural: read the production source if it
//! is present and confirm every variant this crate mirrors still exists there
//! under the same name. If a rename lands on one side only, this fails and
//! someone has to decide which side is right.
//!
//! When the source is absent -- a certification lab has the fixtures but not
//! the repository -- the test reports that and passes. A lab cannot be asked
//! to prove something about a file it was never given.

use std::path::PathBuf;

const BRIDGE_TYPES: &str = "../../codegen/grokptah-agent-bridge/src/computer_use/types.rs";

fn production_source() -> Option<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(BRIDGE_TYPES);
    std::fs::read_to_string(path).ok()
}

/// Variant names this crate mirrors, grouped by the production enum they came
/// from. Kept as text so the check is about names, not about types.
fn mirrored_vocabulary() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        (
            "Sensitivity",
            vec!["None", "Potential", "Secure", "SystemRestricted"],
        ),
        (
            "SemanticAction",
            vec!["Invoke", "SetValue", "Select", "Scroll"],
        ),
        (
            "ActionClass",
            vec!["Semantic", "TextEntry", "KeyChord", "PointerFallback"],
        ),
        (
            "ComputerControlDisposition",
            vec![
                "AgentOwned",
                "Paused",
                "OperatorTakeover",
                "Stopped",
                "Interrupted",
                "UncertainOutcome",
            ],
        ),
        (
            "ComputerErrorCode",
            vec![
                "InvalidRequest",
                "InvalidState",
                "Unauthorized",
                "PermissionRequired",
                "PermissionDenied",
                "PermissionRevoked",
                "ForbiddenTarget",
                "ForbiddenAction",
                "SensitiveSurface",
                "StaleObservation",
                "TargetChanged",
                "TargetClosed",
                "LimitReached",
                "Conflict",
                "UncertainOutcome",
                "Interrupted",
                "BackendUnavailable",
            ],
        ),
        (
            "ComputerKey",
            vec![
                "Enter",
                "Escape",
                "Tab",
                "ArrowUp",
                "ArrowDown",
                "ArrowLeft",
                "ArrowRight",
                "Space",
                "Backspace",
                "Delete",
                "Home",
                "End",
                "PageUp",
                "PageDown",
                "Shift",
                "Control",
                "Alt",
                "Meta",
            ],
        ),
    ]
}

#[test]
fn mirrored_variants_still_exist_in_the_production_vocabulary() {
    let Some(source) = production_source() else {
        eprintln!("note: {BRIDGE_TYPES} is not present in this checkout; skipping the drift check");
        return;
    };

    let mut missing: Vec<String> = Vec::new();
    for (enum_name, variants) in mirrored_vocabulary() {
        for variant in variants {
            // Variants appear as bare identifiers followed by a comma, a
            // brace, or a parenthesis in the production source.
            let present = source.lines().any(|line| {
                let trimmed = line.trim();
                trimmed == format!("{variant},")
                    || trimmed.starts_with(&format!("{variant} {{"))
                    || trimmed.starts_with(&format!("{variant}("))
                    || trimmed == variant
            });
            if !present {
                missing.push(format!("{enum_name}::{variant}"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "the benchmark mirrors variants the production vocabulary no longer has: {missing:?}. \
         Either the rename needs to land here too, or the mirror list is out of date."
    );
}

#[test]
fn the_benchmark_does_not_depend_on_the_implementation_it_judges() {
    let manifest =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("own manifest is readable");
    assert!(
        !manifest.contains("grokptah-agent-bridge"),
        "the qualification harness must not depend on the crate it qualifies"
    );
}

#[test]
fn the_hard_denied_rule_matches_production() {
    let Some(source) = production_source() else {
        eprintln!("note: production source absent; skipping");
        return;
    };
    // Production defines hard-denied as exactly Secure | SystemRestricted.
    // If that widens or narrows, every privacy claim in this crate shifts.
    let normalized: String = source.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalized.contains("matches!(self, Self::Secure | Self::SystemRestricted)"),
        "production's hard-denied definition changed; revisit Sensitivity::is_hard_denied"
    );
}
