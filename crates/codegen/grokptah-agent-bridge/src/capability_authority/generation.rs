//! The generation stamp and the single refusal value.

use std::fmt;

use uuid::Uuid;

/// Every refusal from this module.
///
/// It carries no reason. A foreign authority, an unknown binding, a revoked
/// binding, a stale generation, a drifted digest and an exhausted counter all
/// produce this exact value with this exact message, so a caller holding a
/// binding cannot learn *why* it stopped working — only that it is not
/// current. That is deliberate: a discriminating denial is an oracle for
/// probing which qualifications exist on the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CapabilityDenied;

impl CapabilityDenied {
    /// The one message every refusal carries.
    pub const MESSAGE: &'static str = "computer capability authority is not current";

    pub fn new() -> Self {
        Self
    }
}

impl fmt::Display for CapabilityDenied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(Self::MESSAGE)
    }
}

impl std::error::Error for CapabilityDenied {}

/// Opaque, secret-free stamp naming one live capability authority and its
/// monotonic generation.
///
/// `authority` identifies one live [`super::CapabilityRegistry`]; it is drawn
/// fresh whenever a registry is constructed, so nothing minted before a
/// process restart is ever current afterwards. `counter` is that registry's
/// capability generation.
///
/// The stamp carries no credential material and no capability facts, and its
/// constructors are crate-internal, so holding one does not let a caller build
/// another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CapabilityGeneration {
    authority: Uuid,
    counter: u64,
}

impl CapabilityGeneration {
    /// Terminal counter.
    ///
    /// A generation at this value authorizes nothing and can never advance, so
    /// the counter cannot wrap and a stale binding can never become current
    /// again by lapping the space.
    const EXHAUSTED: u64 = u64::MAX;

    /// First generation of a freshly minted authority.
    ///
    /// Each call draws an authority no other registry can match, so a binding
    /// issued by one registry is never current at another.
    pub(super) fn new_authority() -> Self {
        Self {
            authority: Uuid::new_v4(),
            counter: 0,
        }
    }

    /// Next generation of the same authority.
    ///
    /// Exhaustion fails closed rather than saturating or wrapping: a saturated
    /// counter would make already-issued stale bindings current again, and a
    /// wrapped one would do it silently. The caller must treat the error as a
    /// refusal to mutate at all — see [`super::CapabilityRegistry`], which
    /// computes the next generation *before* it touches any state.
    pub(super) fn next(self) -> Result<Self, CapabilityDenied> {
        if self.is_exhausted() {
            return Err(CapabilityDenied);
        }
        let counter = self.counter.checked_add(1).ok_or(CapabilityDenied)?;
        Ok(Self {
            authority: self.authority,
            counter,
        })
    }

    /// Whether this generation has reached the terminal counter. An exhausted
    /// authority can no longer prove a revocation, so it authorizes nothing.
    pub(super) fn is_exhausted(self) -> bool {
        self.counter == Self::EXHAUSTED
    }

    /// Monotonic counter, exposed for operator-facing diagnostics. The
    /// authority id is deliberately not exposed: it is the half that makes a
    /// foreign binding unforgeable.
    pub fn counter(self) -> u64 {
        self.counter
    }

    /// The same authority pinned one advance short of exhaustion, so the
    /// terminal transition and the refusal after it can be exercised without
    /// 2^64 rotations. Crate-internal; reached only through
    /// [`super::CapabilityRegistry::pin_near_exhaustion_for_test`].
    #[cfg(test)]
    pub(super) fn pinned_near_exhaustion(self) -> Self {
        Self {
            authority: self.authority,
            counter: Self::EXHAUSTED - 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_authority_is_distinct_and_starts_at_zero() {
        let first = CapabilityGeneration::new_authority();
        let second = CapabilityGeneration::new_authority();
        assert_eq!(first.counter(), 0);
        assert_eq!(second.counter(), 0);
        assert_ne!(
            first, second,
            "two registries must never share an authority id"
        );
    }

    #[test]
    fn advance_is_monotonic_and_never_equals_its_predecessor() {
        let first = CapabilityGeneration::new_authority();
        let second = first.next().expect("advance");
        assert_eq!(second.counter(), 1);
        assert_ne!(first, second, "an advance must invalidate the old stamp");
    }

    #[test]
    fn exhaustion_fails_closed_instead_of_wrapping() {
        let near = CapabilityGeneration::new_authority().pinned_near_exhaustion();
        let terminal = near
            .next()
            .expect("last advance lands on the terminal counter");
        assert!(terminal.is_exhausted());
        assert_eq!(
            terminal.next(),
            Err(CapabilityDenied),
            "an exhausted authority must refuse to advance"
        );
        assert_ne!(
            terminal,
            CapabilityGeneration::new_authority(),
            "exhaustion must not wrap back onto a fresh authority"
        );
    }

    #[test]
    fn denial_is_a_single_indistinguishable_value() {
        assert_eq!(CapabilityDenied::new(), CapabilityDenied);
        assert_eq!(
            CapabilityDenied::new().to_string(),
            CapabilityDenied::MESSAGE
        );
    }
}
