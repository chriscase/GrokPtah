//! A deterministic pseudo-random source.
//!
//! The benchmark needs variation without randomness. `rand` would give the
//! second without the first: a trace that cannot be reproduced byte for byte
//! is not evidence, it is an anecdote. So scenarios draw from a splitmix64
//! stream seeded from the scenario identity, which means the same
//! (scenario, profile, tier, horizon) tuple produces the same trace on every
//! machine, every run, forever.
//!
//! Splitmix64 rather than something stronger because the requirement is
//! reproducible spread, not unpredictability. Nothing here is used for
//! anything that needs to resist an adversary.

/// A reproducible stream of `u64`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    /// Seed from an arbitrary label, so a scenario's stream follows its
    /// identity rather than its position in a list.
    #[must_use]
    pub fn from_label(label: &str) -> Self {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        for byte in label.bytes() {
            state = state.rotate_left(7) ^ u64::from(byte);
            state = state.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        }
        Self { state }
    }

    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Next value in the stream.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform-ish value in `0..bound`. Returns 0 when `bound` is 0.
    pub fn below(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % u64::from(bound)) as u32
    }

    /// A value in `low..=high`, clamped when the range is inverted.
    pub fn in_range(&mut self, low: u32, high: u32) -> u32 {
        if high <= low {
            return low;
        }
        low + self.below(high - low + 1)
    }

    /// True with probability `bps / 10_000`.
    pub fn chance_bps(&mut self, bps: u32) -> bool {
        self.below(10_000) < bps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_label_gives_the_same_stream() {
        let mut a = DeterministicRng::from_label("reference/economy/small_local/h30");
        let mut b = DeterministicRng::from_label("reference/economy/small_local/h30");
        for _ in 0..64 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_labels_diverge() {
        let mut a = DeterministicRng::from_label("reference/economy/small_local/h30");
        let mut b = DeterministicRng::from_label("reference/economy/small_local/h300");
        let a_values: Vec<_> = (0..16).map(|_| a.next_u64()).collect();
        let b_values: Vec<_> = (0..16).map(|_| b.next_u64()).collect();
        assert_ne!(a_values, b_values);
    }

    #[test]
    fn bounds_are_respected_including_degenerate_ones() {
        let mut rng = DeterministicRng::from_seed(7);
        for _ in 0..1_000 {
            assert!(rng.below(5) < 5);
            let value = rng.in_range(10, 12);
            assert!((10..=12).contains(&value));
        }
        assert_eq!(rng.below(0), 0);
        assert_eq!(rng.in_range(4, 4), 4);
        assert_eq!(rng.in_range(9, 3), 9);
    }

    #[test]
    fn certain_and_impossible_chances_are_exact() {
        let mut rng = DeterministicRng::from_seed(11);
        for _ in 0..256 {
            assert!(rng.chance_bps(10_000));
            assert!(!rng.chance_bps(0));
        }
    }
}
