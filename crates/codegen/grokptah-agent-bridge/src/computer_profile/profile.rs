//! Canonical adaptive Computer Use profiles and their efficiency budgets (#435).
//!
//! Issue #435 is authoritative about the vocabulary: the product exposes
//! exactly **Economy**, **Balanced**, and **High Assurance**. Historical
//! developer checkouts and unmerged donor branches used `efficient` and
//! `frontier`; those are accepted here only as *ingest compatibility aliases*
//! so existing persisted session metadata and deployment overrides keep
//! deserializing. They are never emitted, never enumerated, and never a fourth
//! or fifth mode. [`AdaptiveProfile::ALL`] has three entries and always will.
//!
//! # What a profile is, and what it is emphatically not
//!
//! A profile is an **efficiency budget**: how many elements and bytes of
//! observation the model is shown, whether geometry or a redacted evidence
//! reference travels with it, how many model calls and repairs the run may
//! spend, and which action classes the model may reach for. Economy exists so
//! a small, cheap, company-hosted model can do useful work on a semantic
//! surface without paying for pixels it cannot read anyway.
//!
//! A profile is **not** a safety level. Every authorization, lease,
//! stale-observation, proposal-validation, redaction, uncertainty, and cleanup
//! rule lives in [`SafetyFloor`], which is a single associated constant with no
//! per-profile constructor. There is no `SafetyFloor::for_profile`, so there is
//! nothing for a profile to override. That is the structural answer to the
//! inversion found in the #453 donor candidate, where the *most* expensive
//! profile switched host verification **off** while the cheapest kept it on: a
//! lexical rename would have made "High Assurance" mean less assurance than
//! "Economy". Verification is not a budget knob, so it does not live in the
//! budget table.
//!
//! # Monotonicity
//!
//! Every numeric budget is proven non-decreasing across
//! `Economy <= Balanced <= HighAssurance` and bounded above by the
//! provider-neutral kernel ceiling, at compile time, in [`assertions`]. A
//! profile can therefore only ever *narrow* the kernel; escalation buys more
//! observation and more attempts, never more authority.

use serde::{Deserialize, Serialize};

/// Canonical wire tokens, in authority order. Reports and persisted records
/// only ever contain these three.
pub const CANONICAL_PROFILE_NAMES: [&str; 3] = ["economy", "balanced", "high_assurance"];

/// How a profile token arrived. Aliases are readable but never writable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileTokenKind {
    /// One of [`CANONICAL_PROFILE_NAMES`].
    Canonical,
    /// A historical `efficient`/`frontier` token from unmerged-runtime
    /// metadata. Canonicalized on read; never produced on write.
    CompatibilityAlias,
}

/// The three canonical execution profiles.
///
/// Ordering is authority order, so `profile >= AdaptiveProfile::Balanced`
/// reads correctly. [`Self::Economy`] is [`Default`] because an unattributed
/// caller must land on the cheapest, most abstaining contract rather than the
/// most generous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum AdaptiveProfile {
    /// Semantic-first, compact observations, no screenshot capture, bounded
    /// action classes, strict budgets, high abstention.
    #[default]
    Economy,
    /// Semantic-first with element geometry and moderate verification budget.
    Balanced,
    /// Strongest eligible path: geometry plus the redacted evidence reference,
    /// the largest budgets, and — additively — an independent verifier
    /// requirement for consequential work.
    HighAssurance,
}

impl AdaptiveProfile {
    /// Every profile the product exposes. Exactly three, forever.
    pub const ALL: [Self; 3] = [Self::Economy, Self::Balanced, Self::HighAssurance];

    /// The canonical wire token. Aliases are never returned here.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Economy => "economy",
            Self::Balanced => "balanced",
            Self::HighAssurance => "high_assurance",
        }
    }

    /// Operator-facing display name.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Economy => "Economy",
            Self::Balanced => "Balanced",
            Self::HighAssurance => "High Assurance",
        }
    }

    /// Parses a profile token, accepting the two compatibility aliases.
    ///
    /// Returns which kind of token it was so a caller that is migrating
    /// persisted data can record that an alias was seen without ever writing
    /// one back.
    pub fn from_ingest_token(raw: &str) -> Option<(Self, ProfileTokenKind)> {
        match raw {
            "economy" => Some((Self::Economy, ProfileTokenKind::Canonical)),
            "balanced" => Some((Self::Balanced, ProfileTokenKind::Canonical)),
            "high_assurance" => Some((Self::HighAssurance, ProfileTokenKind::Canonical)),
            // Ingest compatibility only. `efficient` was the historical name
            // for the cheapest profile and `frontier` for the richest. They do
            // not carry the donor's *semantics*, only its identity: the
            // verification inversion is dropped on the way in.
            "efficient" => Some((Self::Economy, ProfileTokenKind::CompatibilityAlias)),
            "frontier" => Some((Self::HighAssurance, ProfileTokenKind::CompatibilityAlias)),
            _ => None,
        }
    }

    /// The next profile up, or `None` at the ceiling of the ladder.
    pub const fn escalated(self) -> Option<Self> {
        match self {
            Self::Economy => Some(Self::Balanced),
            Self::Balanced => Some(Self::HighAssurance),
            Self::HighAssurance => None,
        }
    }

    /// The profile's efficiency budget. A `const` table, not a computed policy,
    /// so two runs of the same profile always admit exactly the same inputs.
    pub const fn budget(self) -> ProfileBudget {
        match self {
            Self::Economy => ECONOMY_BUDGET,
            Self::Balanced => BALANCED_BUDGET,
            Self::HighAssurance => HIGH_ASSURANCE_BUDGET,
        }
    }

    /// The safety rules in force. Identical for every profile by construction:
    /// this method takes `self` only so call sites read naturally, and the
    /// `profile_independent_safety_floor` test proves it discards it.
    pub const fn safety_floor(self) -> SafetyFloor {
        SafetyFloor::REQUIRED
    }
}

impl std::fmt::Display for AdaptiveProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for AdaptiveProfile {
    /// Always canonical. There is no code path that writes an alias.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AdaptiveProfile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = std::borrow::Cow::<'de, str>::deserialize(deserializer)?;
        Self::from_ingest_token(raw.as_ref())
            .map(|(profile, _)| profile)
            .ok_or_else(|| {
                serde::de::Error::custom(format!(
                    "unknown Computer Use profile {raw:?}; expected one of {}",
                    CANONICAL_PROFILE_NAMES.join(", ")
                ))
            })
    }
}

/// How much of an observation a profile may render for the model.
///
/// No variant admits screenshot **bytes**. High Assurance is allowed the
/// redacted evidence reference (content hash, media type, dimensions) because
/// that is already non-reversible metadata the operator has approved; the
/// pixels stay host-side in every profile, which is why
/// [`Self::allows_screenshot_bytes`] is a constant `false` rather than a
/// per-variant answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationDetail {
    /// Role, label, value, enabled/focused, and the advertised action set.
    /// No geometry, no evidence reference, no bytes.
    SemanticOnly,
    /// Adds per-element geometry, which is enough to reconstruct layout.
    SemanticWithGeometry,
    /// Adds the redacted screenshot reference. Never the screenshot itself.
    SemanticWithEvidenceRef,
}

impl ObservationDetail {
    pub const fn allows_geometry(self) -> bool {
        matches!(
            self,
            Self::SemanticWithGeometry | Self::SemanticWithEvidenceRef
        )
    }

    pub const fn allows_evidence_reference(self) -> bool {
        matches!(self, Self::SemanticWithEvidenceRef)
    }

    /// Screenshot bytes never cross the model boundary in any profile.
    pub const fn allows_screenshot_bytes(self) -> bool {
        false
    }
}

/// Per-profile efficiency ceilings.
///
/// Every field here answers "how much may this cost?". Nothing here answers
/// "is this allowed?" — that is [`SafetyFloor`]'s job, and keeping the two
/// types disjoint is what makes "no profile bypasses the safety path" a
/// structural claim rather than a review promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileBudget {
    pub observation_detail: ObservationDetail,
    /// Elements rendered for the model. Beyond this the host applies
    /// deterministic bounded candidate ranking and marks the rendered payload
    /// truncated, so the model chooses among fewer candidates rather than
    /// seeing a silently different view.
    pub max_observation_elements: u32,
    /// Serialized bytes of the rendered observation payload.
    pub max_observation_bytes: u64,
    /// Per-element label/value bytes in the rendered payload.
    pub max_element_text_bytes: u32,
    /// Model calls this run may spend in total, across proposals and repairs.
    pub max_model_calls: u32,
    /// Raw model response bytes. Always checkable, unlike token counts.
    pub max_response_bytes: u64,
    /// Wall-clock budget for one proposal turn including its repairs.
    pub max_turn_millis: u64,
    /// Bytes the model may ask to type. Never above the kernel's own limit.
    pub max_text_entry_bytes: u32,
    /// Absolute scroll delta the model may request on either axis.
    pub max_scroll_delta: i32,
    /// Bytes of operator-facing proposal summary.
    pub max_summary_bytes: u32,
    /// Whether the model may reach for pointer-fallback actions. Still subject
    /// to the grant's own action classes, which the kernel checks separately.
    pub allows_pointer_fallback: bool,
    /// Whether the model may reach for key-chord actions. Same caveat.
    pub allows_key_chord: bool,
}

/// Economy: semantic-only, screenshot-free, tightest budgets, most abstention.
pub const ECONOMY_BUDGET: ProfileBudget = ProfileBudget {
    observation_detail: ObservationDetail::SemanticOnly,
    max_observation_elements: 48,
    max_observation_bytes: 24 * 1024,
    max_element_text_bytes: 256,
    max_model_calls: 16,
    max_response_bytes: 8 * 1024,
    max_turn_millis: 20_000,
    max_text_entry_bytes: 1_024,
    max_scroll_delta: 2_000,
    max_summary_bytes: 256,
    allows_pointer_fallback: false,
    allows_key_chord: false,
};

/// Balanced: semantic-first with geometry and a moderate verification budget.
pub const BALANCED_BUDGET: ProfileBudget = ProfileBudget {
    observation_detail: ObservationDetail::SemanticWithGeometry,
    max_observation_elements: 256,
    max_observation_bytes: 128 * 1024,
    max_element_text_bytes: 512,
    max_model_calls: 48,
    max_response_bytes: 32 * 1024,
    max_turn_millis: 60_000,
    max_text_entry_bytes: 4 * 1024,
    max_scroll_delta: 10_000,
    max_summary_bytes: 512,
    allows_pointer_fallback: true,
    allows_key_chord: false,
};

/// High Assurance: richest eligible observation and the largest budgets. Note
/// that nothing here relaxes a check — the additional assurance obligations
/// live in [`SafetyFloor`] and [`AdaptiveProfile::requires_independent_verifier`].
pub const HIGH_ASSURANCE_BUDGET: ProfileBudget = ProfileBudget {
    observation_detail: ObservationDetail::SemanticWithEvidenceRef,
    max_observation_elements: 1_024,
    max_observation_bytes: 512 * 1024,
    max_element_text_bytes: 512,
    max_model_calls: 96,
    max_response_bytes: 128 * 1024,
    max_turn_millis: 120_000,
    max_text_entry_bytes: KERNEL_TEXT_ENTRY_BYTES,
    max_scroll_delta: KERNEL_SCROLL_DELTA,
    max_summary_bytes: 512,
    allows_pointer_fallback: true,
    allows_key_chord: true,
};

/// The safety rules that hold in **every** profile.
///
/// There is deliberately no `for_profile` constructor and no public field
/// mutation: [`Self::REQUIRED`] is the only value of this type a caller can
/// obtain. A profile therefore has no mechanism by which to relax any of these,
/// which is the structural form of issue #435's "Economy mode is an efficiency
/// policy, never a reduced-safety mode".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyFloor {
    /// The host's own account of the current frame must agree with the
    /// model's claimed observation before a proposal is staged. The model's
    /// account of what it is looking at is never the authority.
    pub requires_host_verification: bool,
    /// A proposal binds to one exact observation id **and** sequence.
    pub requires_fresh_observation_binding: bool,
    /// A completion claim is accepted only for the exact run version and
    /// observation it was made against, so "done" cannot be carried forward
    /// from an earlier frame. This is a binding rule, not a verification one:
    /// whether an *independent* verifier confirmed the postcondition is
    /// separate evidence, reported as
    /// `HostCapabilityEvidence::independent_verifier`, and this build does not
    /// have one.
    pub requires_completion_bound_to_current_observation: bool,
    /// Screenshot bytes never reach a model in any profile.
    pub allows_screenshot_bytes_to_model: bool,
    /// Actions come only from the closed typed grammar, never from prose.
    pub allows_free_form_action: bool,
    /// An action already dispatched with an untrusted outcome is never
    /// replayed automatically.
    pub allows_automatic_replay_after_uncertain_dispatch: bool,
    /// Identical consecutive frames tolerated before the run must escalate or
    /// stop rather than repeat itself.
    pub max_stationary_repeats: u32,
    /// Consecutive unusable or low-confidence answers tolerated before the run
    /// must escalate or stop.
    pub max_consecutive_uncertain_answers: u32,
    /// A plan whose postcondition was contradicted may escalate once. A second
    /// failure halts instead of escalating again.
    pub max_verification_failures: u32,
}

impl SafetyFloor {
    /// The one and only safety floor. Not parameterized by profile, model,
    /// provider, cost, or operator preference.
    pub const REQUIRED: Self = Self {
        requires_host_verification: true,
        requires_fresh_observation_binding: true,
        requires_completion_bound_to_current_observation: true,
        allows_screenshot_bytes_to_model: false,
        allows_free_form_action: false,
        allows_automatic_replay_after_uncertain_dispatch: false,
        max_stationary_repeats: 2,
        max_consecutive_uncertain_answers: 2,
        max_verification_failures: 2,
    };
}

impl AdaptiveProfile {
    /// High Assurance additionally requires a verifier independent of the
    /// model that proposed the action. This is *additive*: it is the only
    /// profile-varying assurance rule, and it only ever adds an obligation.
    pub const fn requires_independent_verifier(self) -> bool {
        matches!(self, Self::HighAssurance)
    }
}

/// Kernel bounds mirrored as constants so the monotonicity proof below can run
/// in `const` context. [`assertions::mirrors_match_kernel_ceiling`] fails the
/// test suite if the kernel ever moves underneath us.
const KERNEL_TEXT_ENTRY_BYTES: u32 = crate::computer_use::MAX_TEXT_ENTRY_BYTES as u32;
const KERNEL_SCROLL_DELTA: i32 = 10_000;
const KERNEL_LABEL_BYTES: u32 = crate::computer_use::MAX_LABEL_BYTES as u32;
const KERNEL_SEMANTIC_ELEMENTS: u32 = 10_000;
const KERNEL_SEMANTIC_BYTES: u64 = 8 * 1024 * 1024;
const KERNEL_DURATION_MILLIS: u64 = 60 * 60 * 1_000;
/// The wire schema bounds the operator-facing summary at 512 bytes.
const OPERATOR_SUMMARY_BYTES: u32 = 512;

/// Compile-time proofs. Nothing here runs at startup; a violation is a build
/// failure, which is what "no profile bypasses the safety path" has to mean if
/// it is to survive a future edit to the budget table.
mod assertions {
    use super::*;

    /// Every budget narrows the provider-neutral kernel.
    const fn within_kernel(budget: &ProfileBudget) -> bool {
        budget.max_text_entry_bytes > 0
            && budget.max_text_entry_bytes <= KERNEL_TEXT_ENTRY_BYTES
            && budget.max_scroll_delta > 0
            && budget.max_scroll_delta <= KERNEL_SCROLL_DELTA
            && budget.max_observation_elements > 0
            && budget.max_observation_elements <= KERNEL_SEMANTIC_ELEMENTS
            && budget.max_observation_bytes > 0
            && budget.max_observation_bytes <= KERNEL_SEMANTIC_BYTES
            && budget.max_element_text_bytes > 0
            && budget.max_element_text_bytes <= KERNEL_LABEL_BYTES
            && budget.max_summary_bytes > 0
            && budget.max_summary_bytes <= OPERATOR_SUMMARY_BYTES
            && budget.max_turn_millis > 0
            && budget.max_turn_millis <= KERNEL_DURATION_MILLIS
            && budget.max_model_calls > 0
            && budget.max_response_bytes > 0
    }

    /// `lower` spends no more than `higher` on any axis.
    const fn narrows(lower: &ProfileBudget, higher: &ProfileBudget) -> bool {
        (lower.observation_detail as u8) <= (higher.observation_detail as u8)
            && lower.max_observation_elements <= higher.max_observation_elements
            && lower.max_observation_bytes <= higher.max_observation_bytes
            && lower.max_element_text_bytes <= higher.max_element_text_bytes
            && lower.max_model_calls <= higher.max_model_calls
            && lower.max_response_bytes <= higher.max_response_bytes
            && lower.max_turn_millis <= higher.max_turn_millis
            && lower.max_text_entry_bytes <= higher.max_text_entry_bytes
            && lower.max_scroll_delta <= higher.max_scroll_delta
            && lower.max_summary_bytes <= higher.max_summary_bytes
            && implies(
                lower.allows_pointer_fallback,
                higher.allows_pointer_fallback,
            )
            && implies(lower.allows_key_chord, higher.allows_key_chord)
    }

    /// A permission the cheaper profile has, the richer one must also have.
    const fn implies(lower: bool, higher: bool) -> bool {
        !lower || higher
    }

    const _: () = assert!(
        within_kernel(&ECONOMY_BUDGET)
            && within_kernel(&BALANCED_BUDGET)
            && within_kernel(&HIGH_ASSURANCE_BUDGET),
        "an adaptive profile budget escapes the provider-neutral kernel ceiling"
    );

    const _: () = assert!(
        narrows(&ECONOMY_BUDGET, &BALANCED_BUDGET)
            && narrows(&BALANCED_BUDGET, &HIGH_ASSURANCE_BUDGET),
        "adaptive profile budgets are not monotonic in authority order"
    );

    const _: () = assert!(
        !ECONOMY_BUDGET.observation_detail.allows_screenshot_bytes()
            && !BALANCED_BUDGET.observation_detail.allows_screenshot_bytes()
            && !HIGH_ASSURANCE_BUDGET
                .observation_detail
                .allows_screenshot_bytes(),
        "a profile admits screenshot bytes across the model boundary"
    );

    const _: () = assert!(
        SafetyFloor::REQUIRED.requires_host_verification
            && SafetyFloor::REQUIRED.requires_fresh_observation_binding
            && SafetyFloor::REQUIRED.requires_completion_bound_to_current_observation
            && !SafetyFloor::REQUIRED.allows_screenshot_bytes_to_model
            && !SafetyFloor::REQUIRED.allows_free_form_action
            && !SafetyFloor::REQUIRED.allows_automatic_replay_after_uncertain_dispatch,
        "the single safety floor no longer holds the #435 invariants"
    );

    /// Economy is the cheapest profile on every axis, so escalation can only
    /// ever buy more observation and more attempts.
    const _: () = assert!(
        ECONOMY_BUDGET.observation_detail as u8 == ObservationDetail::SemanticOnly as u8
            && !ECONOMY_BUDGET.observation_detail.allows_screenshot_bytes(),
        "Economy is no longer the semantic-first, screenshot-free profile"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_use::ComputerUseLimits;

    #[test]
    fn mirrors_match_kernel_ceiling() {
        let kernel = ComputerUseLimits::ceiling();
        assert_eq!(KERNEL_TEXT_ENTRY_BYTES, kernel.max_text_entry_bytes);
        assert_eq!(KERNEL_SEMANTIC_ELEMENTS, kernel.max_semantic_elements);
        assert_eq!(KERNEL_SEMANTIC_BYTES, kernel.max_semantic_bytes);
        assert_eq!(
            KERNEL_DURATION_MILLIS,
            kernel.max_duration_secs.saturating_mul(1_000)
        );
    }

    #[test]
    fn profile_independent_safety_floor() {
        for profile in AdaptiveProfile::ALL {
            assert_eq!(
                profile.safety_floor(),
                SafetyFloor::REQUIRED,
                "{profile} obtained a different safety floor"
            );
        }
    }

    #[test]
    fn aliases_are_ingest_only_and_never_emitted() {
        assert_eq!(
            AdaptiveProfile::from_ingest_token("efficient"),
            Some((
                AdaptiveProfile::Economy,
                ProfileTokenKind::CompatibilityAlias
            ))
        );
        assert_eq!(
            AdaptiveProfile::from_ingest_token("frontier"),
            Some((
                AdaptiveProfile::HighAssurance,
                ProfileTokenKind::CompatibilityAlias
            ))
        );
        for profile in AdaptiveProfile::ALL {
            let wire = serde_json::to_string(&profile).unwrap();
            assert!(
                !wire.contains("efficient") && !wire.contains("frontier"),
                "{profile} serialized to an alias: {wire}"
            );
            assert!(CANONICAL_PROFILE_NAMES.contains(&profile.as_str()));
        }
        // An alias round-trips *into* the canonical token, never back out.
        let ingested: AdaptiveProfile = serde_json::from_str("\"frontier\"").unwrap();
        assert_eq!(ingested, AdaptiveProfile::HighAssurance);
        assert_eq!(
            serde_json::to_string(&ingested).unwrap(),
            "\"high_assurance\""
        );
    }

    #[test]
    fn aliases_do_not_invent_extra_modes() {
        assert_eq!(AdaptiveProfile::ALL.len(), 3);
        assert_eq!(CANONICAL_PROFILE_NAMES.len(), 3);
        let distinct: std::collections::BTreeSet<_> = ["economy", "balanced", "high_assurance"]
            .into_iter()
            .chain(["efficient", "frontier"])
            .filter_map(AdaptiveProfile::from_ingest_token)
            .map(|(profile, _)| profile)
            .collect();
        assert_eq!(distinct.len(), 3, "ingest produced more than three modes");
    }

    #[test]
    fn unknown_profile_tokens_fail_closed() {
        assert!(AdaptiveProfile::from_ingest_token("cheap").is_none());
        assert!(AdaptiveProfile::from_ingest_token("Economy").is_none());
        assert!(AdaptiveProfile::from_ingest_token("").is_none());
        assert!(serde_json::from_str::<AdaptiveProfile>("\"ludicrous\"").is_err());
        assert!(serde_json::from_str::<AdaptiveProfile>("3").is_err());
    }

    #[test]
    fn default_is_the_strictest_and_cheapest_profile() {
        assert_eq!(AdaptiveProfile::default(), AdaptiveProfile::Economy);
        assert!(AdaptiveProfile::Economy < AdaptiveProfile::Balanced);
        assert!(AdaptiveProfile::Balanced < AdaptiveProfile::HighAssurance);
    }

    #[test]
    fn escalation_ladder_terminates() {
        assert_eq!(
            AdaptiveProfile::Economy.escalated(),
            Some(AdaptiveProfile::Balanced)
        );
        assert_eq!(
            AdaptiveProfile::Balanced.escalated(),
            Some(AdaptiveProfile::HighAssurance)
        );
        assert_eq!(AdaptiveProfile::HighAssurance.escalated(), None);
    }

    #[test]
    fn only_high_assurance_adds_an_obligation() {
        assert!(!AdaptiveProfile::Economy.requires_independent_verifier());
        assert!(!AdaptiveProfile::Balanced.requires_independent_verifier());
        assert!(AdaptiveProfile::HighAssurance.requires_independent_verifier());
        // The donor inversion, restated as a test: the richest profile must
        // never verify *less* than the cheapest one.
        assert_eq!(
            AdaptiveProfile::HighAssurance
                .safety_floor()
                .requires_host_verification,
            AdaptiveProfile::Economy
                .safety_floor()
                .requires_host_verification
        );
    }
}
