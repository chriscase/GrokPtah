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

use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest, Sha256};

/// Domain separator. Bumping this invalidates comparison against digests
/// written by an older build, which is the intended effect of changing what a
/// digest means.
const OBSERVATION_DOMAIN: &[u8] = b"grokptah.durable.observation.v1\n";

/// Bytes of a digest fingerprint exposed in public projections.
///
/// A projection shows enough to correlate two observations and never enough to
/// reconstruct one.
const FINGERPRINT_BYTES: usize = 8;

/// SHA-256 over the *raw* bytes of one observation.
///
/// Deliberately not `Display`: rendering a full digest into operator prose is
/// how a durable identifier ends up in a log line that outlives its retention
/// policy. Use [`RawObservationDigest::fingerprint`] for anything public.
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

    /// Short, non-invertible correlation handle for public projections.
    pub fn fingerprint(&self) -> String {
        self.0[..FINGERPRINT_BYTES].iter().fold(
            String::with_capacity(FINGERPRINT_BYTES * 2),
            |mut acc, b| {
                use fmt::Write as _;
                let _ = write!(acc, "{b:02x}");
                acc
            },
        )
    }

    fn to_hex(self) -> String {
        self.0.iter().fold(String::with_capacity(64), |mut acc, b| {
            use fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        })
    }

    fn from_hex(text: &str) -> Option<Self> {
        if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut out = [0u8; 32];
        for (index, slot) in out.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
        }
        Some(Self(out))
    }
}

impl fmt::Debug for RawObservationDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RawObservationDigest({}…)", self.fingerprint())
    }
}

impl Serialize for RawObservationDigest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for RawObservationDigest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_hex(&text)
            .ok_or_else(|| D::Error::custom("observation digest must be 64 lowercase hex digits"))
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

/// Why a bounded step ended, as a typed value rather than parsed prose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TerminalObservation {
    /// The step ran and produced output.
    Produced {
        digest: RawObservationDigest,
        raw_len: usize,
        truncated: bool,
    },
    /// The tool ran and reported its own failure.
    ToolFailed {
        digest: RawObservationDigest,
        raw_len: usize,
    },
    /// The step was refused before it ran. Nothing executed.
    Refused { reason: RefusalReason },
    /// The step was cancelled. Whether it had already taken effect is recorded
    /// separately; this value never asserts that it did not.
    Cancelled { effect_may_have_landed: bool },
}

impl TerminalObservation {
    /// Build the terminal observation for a step that produced output.
    pub fn produced(projection: &BoundedProjection) -> Self {
        Self::Produced {
            digest: projection.raw_digest(),
            raw_len: projection.raw_len(),
            truncated: projection.truncated(),
        }
    }

    /// Build the terminal observation for a tool that reported failure.
    pub fn tool_failed(observation: &RawObservation) -> Self {
        Self::ToolFailed {
            digest: observation.digest(),
            raw_len: observation.raw_len(),
        }
    }

    /// The raw digest, when the step got far enough to have one.
    pub fn digest(&self) -> Option<RawObservationDigest> {
        match self {
            Self::Produced { digest, .. } | Self::ToolFailed { digest, .. } => Some(*digest),
            Self::Refused { .. } | Self::Cancelled { .. } => None,
        }
    }

    /// Whether this step is known to have executed.
    ///
    /// `Cancelled` answers `false` only when the host can prove nothing landed;
    /// otherwise it stays honest about not knowing.
    pub fn definitely_did_not_execute(&self) -> bool {
        match self {
            Self::Refused { .. } => true,
            Self::Cancelled {
                effect_may_have_landed,
            } => !effect_may_have_landed,
            Self::Produced { .. } | Self::ToolFailed { .. } => false,
        }
    }
}

/// Why a step was refused before execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// The caller holds no grant for this effect.
    NotAuthorized,
    /// A bound (rounds, duration, tokens, bytes) was already spent.
    BoundExhausted,
    /// The host is shutting down and refuses new admissions.
    Quiescing,
    /// The work item's revision moved under the caller.
    StaleRevision,
}
