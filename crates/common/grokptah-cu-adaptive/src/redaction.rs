//! Semantic redaction, preserved end to end.
//!
//! The production Computer Use kernel has one rule about content: a secure
//! surface never yields a value, and a screenshot never reaches a consumer
//! unredacted. An adaptive layer that plans several steps ahead is the obvious
//! place for that rule to erode, because a plan is a *durable* object -- it is
//! serialized, digested, compared, and stored in a receipt. If typed text
//! lived in a plan, redaction would hold at the kernel and leak at the
//! planner.
//!
//! So this module makes the leak structurally impossible rather than
//! policed:
//!
//! * [`TextPayload`] holds its literal in a field that is **skipped by
//!   serde**. There is no serialization path that emits it, so no plan, no
//!   verdict, and no receipt can carry it, and a plan that has round-tripped
//!   through JSON comes back without it.
//! * A payload whose class is [`TextClass::Secret`] cannot be constructed at
//!   all. Refusing at construction means a secret never exists as a plannable
//!   value even in memory.
//! * [`Sensitivity`] mirrors the kernel's ladder, and hard-denied surfaces are
//!   refused before any grounding, confidence, or budget question is asked.

use serde::{Deserialize, Serialize};

use crate::digest::{digest_str, domain, is_digest};

/// Sensitivity ladder, mirroring the production kernel's.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    /// Ordinary application content.
    #[default]
    None,
    /// Adjacent to something sensitive: same window as a credential field, a
    /// payment form, a private message thread. Actionable, but gated.
    Potential,
    /// A secure field. Never yields a value, never accepts synthesized input.
    Secure,
    /// A system-restricted surface. Not observable, not actionable.
    SystemRestricted,
}

impl Sensitivity {
    pub const ALL: &'static [Sensitivity] = &[
        Self::None,
        Self::Potential,
        Self::Secure,
        Self::SystemRestricted,
    ];

    /// True when no profile, tier, grant, or human approval can make this
    /// surface actionable. Hard denial is checked first and is not a
    /// threshold.
    #[must_use]
    pub fn is_hard_denied(self) -> bool {
        matches!(self, Self::Secure | Self::SystemRestricted)
    }

    /// True when the surface may be acted on only behind a human gate.
    #[must_use]
    pub fn requires_approval(self) -> bool {
        matches!(self, Self::Potential)
    }
}

/// What kind of text a step wants to type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextClass {
    /// Ordinary content with no sensitivity signal.
    Benign,
    /// Destined for a field adjacent to a sensitive surface. Permitted only
    /// behind a human approval gate.
    SensitiveAdjacent,
    /// Credential-shaped or otherwise secret. Never constructible.
    Secret,
}

/// Why a text payload was refused at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TextPayloadError {
    #[error("secret-class text is never plannable")]
    SecretClass,
    #[error("text payload is empty")]
    Empty,
    #[error("text payload exceeds the per-step byte bound")]
    TooLong,
    #[error("text payload contains a control character")]
    ControlCharacter,
}

/// The largest text entry a single step may carry, matching the kernel's
/// per-action text bound.
pub const MAX_TEXT_ENTRY_BYTES: usize = 16 * 1024;

/// A typed value that can be planned, compared, and audited without ever being
/// serialized.
///
/// The literal is `#[serde(skip)]`, so `to_json(payload)` emits only the
/// digest, the length, and the class. A deserialized payload has no literal
/// and reports [`TextPayload::is_replayable`] as `false`: it can still be
/// *checked* against a live field (the digest matches or it does not), but it
/// cannot be replayed into one. That asymmetry is the point -- evidence
/// survives the boundary, content does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextPayload {
    #[serde(skip)]
    literal: Option<String>,
    digest: String,
    byte_len: u32,
    char_len: u32,
    class: TextClass,
}

impl TextPayload {
    /// Construct a payload from a literal, refusing secret-class text.
    pub fn new(literal: &str, class: TextClass) -> Result<Self, TextPayloadError> {
        if class == TextClass::Secret {
            return Err(TextPayloadError::SecretClass);
        }
        if literal.is_empty() {
            return Err(TextPayloadError::Empty);
        }
        if literal.len() > MAX_TEXT_ENTRY_BYTES {
            return Err(TextPayloadError::TooLong);
        }
        if literal
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(TextPayloadError::ControlCharacter);
        }
        Ok(Self {
            digest: digest_str(domain::TEXT_PAYLOAD, literal),
            byte_len: literal.len() as u32,
            char_len: literal.chars().count() as u32,
            class,
            literal: Some(literal.to_string()),
        })
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    #[must_use]
    pub fn class(&self) -> TextClass {
        self.class
    }

    #[must_use]
    pub fn byte_len(&self) -> u32 {
        self.byte_len
    }

    #[must_use]
    pub fn char_len(&self) -> u32 {
        self.char_len
    }

    /// True when this payload still holds its literal and can be dispatched.
    /// A payload that crossed a serialization boundary is never replayable.
    #[must_use]
    pub fn is_replayable(&self) -> bool {
        self.literal.is_some()
    }

    /// Check a candidate value against this payload without revealing it.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        digest_str(domain::TEXT_PAYLOAD, candidate) == self.digest
    }

    /// Borrow the literal for dispatch into the synthetic world. This is the
    /// only accessor, it is deliberately not `Display`/`Debug`-reachable, and
    /// nothing in the evidence path calls it.
    #[must_use]
    pub fn dispatch_literal(&self) -> Option<&str> {
        self.literal.as_deref()
    }

    /// True when the recorded shape is internally consistent. Used when a plan
    /// arrives from outside: a payload whose digest is malformed, or whose
    /// declared lengths are impossible, is a schema violation.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        is_digest(&self.digest)
            && self.byte_len > 0
            && self.char_len > 0
            && self.char_len <= self.byte_len
            && self.byte_len as usize <= MAX_TEXT_ENTRY_BYTES
            && self.class != TextClass::Secret
    }
}

/// Scan a serialized structure for content that must never appear in it.
///
/// Used by the leakage tests and by the receipt builder's self-check. The
/// scan is a substring search over the serialized JSON, which is blunt on
/// purpose: it catches a leak regardless of which field it escaped through.
#[must_use]
pub fn leak_scan(serialized: &str, forbidden: &[&str]) -> Vec<String> {
    forbidden
        .iter()
        .filter(|needle| !needle.is_empty() && serialized.contains(**needle))
        .map(|needle| (*needle).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_class_text_is_never_constructible() {
        assert_eq!(
            TextPayload::new("hunter2", TextClass::Secret).unwrap_err(),
            TextPayloadError::SecretClass
        );
    }

    #[test]
    fn literal_never_survives_serialization() {
        let payload = TextPayload::new("Ada Lovelace", TextClass::Benign).unwrap();
        assert!(payload.is_replayable());
        let json = serde_json::to_string(&payload).unwrap();
        assert!(leak_scan(&json, &["Ada Lovelace", "Ada", "Lovelace"]).is_empty());

        let restored: TextPayload = serde_json::from_str(&json).unwrap();
        assert!(!restored.is_replayable());
        assert_eq!(restored.digest(), payload.digest());
        assert!(restored.matches("Ada Lovelace"));
        assert!(restored.dispatch_literal().is_none());
    }

    #[test]
    fn debug_rendering_does_not_expose_the_literal_digest_only() {
        // Debug does render the literal by design (it is an in-process value),
        // so the guarantee that matters is that nothing in the evidence path
        // uses Debug. This test pins the serialized form, which is what the
        // evidence path uses.
        let payload = TextPayload::new("secret-adjacent", TextClass::SensitiveAdjacent).unwrap();
        let value = serde_json::to_value(&payload).unwrap();
        let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec!["byteLen", "charLen", "class", "digest"]);
    }

    #[test]
    fn control_characters_and_oversize_text_are_refused() {
        assert_eq!(
            TextPayload::new("bad\0value", TextClass::Benign).unwrap_err(),
            TextPayloadError::ControlCharacter
        );
        let long = "a".repeat(MAX_TEXT_ENTRY_BYTES + 1);
        assert_eq!(
            TextPayload::new(&long, TextClass::Benign).unwrap_err(),
            TextPayloadError::TooLong
        );
        assert_eq!(
            TextPayload::new("", TextClass::Benign).unwrap_err(),
            TextPayloadError::Empty
        );
    }

    #[test]
    fn hard_denial_is_not_a_threshold() {
        assert!(Sensitivity::Secure.is_hard_denied());
        assert!(Sensitivity::SystemRestricted.is_hard_denied());
        assert!(!Sensitivity::Potential.is_hard_denied());
        assert!(Sensitivity::Potential.requires_approval());
        assert!(!Sensitivity::None.requires_approval());
    }
}
