//! Typed terminal observations, raw digests, and bounded projections.
//!
//! The ordering rule this module exists to enforce: **an observation is
//! digested from its raw bytes before anything bounds, truncates, or
//! summarises it.** A digest taken from a bounded projection cannot see a
//! change that falls outside the bound, so two genuinely different results
//! compare equal and a progressing run looks stationary.
//!
//! That is not hypothetical. `host.rs` caps tool output at 24,000 bytes before
//! it reaches the wire transcript, so a digest computed over the transcript is
//! blind to every byte after the cap. The type system here makes the correct
//! order the only expressible one: a [`BoundedProjection`] can only be
//! obtained *from* a [`RawObservation`], which computes its digest on
//! construction.

use std::fmt;

use sha2::{Digest, Sha256};

/// Domain separator. Bumping this invalidates comparison against digests
/// written by an older build, which is the intended effect of changing what a
/// digest means.
const OBSERVATION_DOMAIN: &[u8] = b"grokptah.durable.observation.v1\n";

/// SHA-256 over the *raw* bytes of one observation.
///
/// Deliberately opaque: no `Serialize`, no `Deserialize`, no `Display`, and no
/// accessor for the bytes. The type system is what keeps this out of a durable
/// record or a read projection — it cannot be written to one.
///
/// That matters because the digest is taken over real tool-result content. A
/// value that never leaves the process is only ever compared against the
/// previous round; a value that can be persisted or projected is a confirmation
/// oracle for anyone who can guess the output. Only the first property is
/// claimed here, and only because the second is unreachable.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RawObservationDigest([u8; 32]);

impl RawObservationDigest {
    /// Digest raw observation bytes.
    ///
    /// Length-prefixed under a domain separator so that concatenation cannot
    /// forge equality between different observation sequences.
    pub fn of_raw(raw: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(OBSERVATION_DOMAIN);
        hasher.update((raw.len() as u64).to_be_bytes());
        hasher.update(raw);
        Self(hasher.finalize().into())
    }

    /// Digest an ordered sequence of raw observations as one unit.
    pub fn of_raw_sequence<'a>(parts: impl IntoIterator<Item = &'a [u8]>) -> Option<Self> {
        let parts: Vec<&[u8]> = parts.into_iter().collect();
        if parts.is_empty() {
            return None;
        }
        let mut hasher = Sha256::new();
        hasher.update(OBSERVATION_DOMAIN);
        hasher.update((parts.len() as u64).to_be_bytes());
        for part in parts {
            hasher.update((part.len() as u64).to_be_bytes());
            hasher.update(part);
        }
        Some(Self(hasher.finalize().into()))
    }

    /// Combine an ordered set of per-step digests into one round digest.
    pub fn of_digests(parts: &[RawObservationDigest]) -> Option<Self> {
        if parts.is_empty() {
            return None;
        }
        let mut hasher = Sha256::new();
        hasher.update(OBSERVATION_DOMAIN);
        hasher.update(b"digest-sequence\n");
        hasher.update((parts.len() as u64).to_be_bytes());
        for part in parts {
            hasher.update(part.0);
        }
        Some(Self(hasher.finalize().into()))
    }
}

impl fmt::Debug for RawObservationDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the value: a debug log is an exposure channel too.
        f.write_str("RawObservationDigest(<redacted>)")
    }
}

/// A tool or model observation, captured before anything bounds it.
///
/// Construction digests the raw bytes, so a value of this type is proof that
/// the digest was taken at the raw boundary.
#[derive(Clone)]
pub struct RawObservation {
    raw: String,
    digest: RawObservationDigest,
}

impl RawObservation {
    /// Capture raw output at the dispatch boundary.
    pub fn capture(raw: impl Into<String>) -> Self {
        let raw = raw.into();
        let digest = RawObservationDigest::of_raw(raw.as_bytes());
        Self { raw, digest }
    }

    /// Digest of the raw bytes — never of a projection of them.
    pub fn digest(&self) -> RawObservationDigest {
        self.digest
    }

    /// Length in bytes of the raw observation, before any bound.
    pub fn raw_len(&self) -> usize {
        self.raw.len()
    }

    /// The raw text. Callers that put this on a wire want [`Self::project`].
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Bound this observation for the wire, carrying the raw digest with it.
    ///
    /// This is the only way to build a [`BoundedProjection`], which is what
    /// makes "digest first, then bound" the only expressible order.
    pub fn project(&self, max_bytes: usize) -> BoundedProjection {
        let head = crate::textutil::truncate_at_char_boundary(&self.raw, max_bytes);
        let truncated = head.len() < self.raw.len();
        let text = if truncated {
            format!("{head}…\n(truncated {} bytes)", self.raw.len())
        } else {
            self.raw.clone()
        };
        BoundedProjection {
            text,
            raw_digest: self.digest,
            raw_len: self.raw.len(),
            truncated,
        }
    }
}

impl fmt::Debug for RawObservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawObservation")
            .field("raw_len", &self.raw.len())
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

/// A bounded rendering of a [`RawObservation`], carrying the raw digest.
#[derive(Clone, Debug)]
pub struct BoundedProjection {
    text: String,
    raw_digest: RawObservationDigest,
    raw_len: usize,
    truncated: bool,
}

impl BoundedProjection {
    /// The bounded text, suitable for the model wire.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Consume the projection for the wire.
    pub fn into_text(self) -> String {
        self.text
    }

    /// Digest of the *raw* observation this was projected from.
    pub fn raw_digest(&self) -> RawObservationDigest {
        self.raw_digest
    }

    /// Byte length of the raw observation before bounding.
    pub fn raw_len(&self) -> usize {
        self.raw_len
    }

    /// Whether bounding discarded bytes.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}
