//! Independent reconstruction of the required evaluation matrix.

use crate::catalog::Scenario;
use crate::types::{
    validate_repeats, AdapterId, EvalError, EvalResult, FamilyId, ProfileId, MAX_REPEATS,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EpisodeIdentity {
    pub scenario_id: String,
    pub family: FamilyId,
    pub profile: ProfileId,
    pub adapter: AdapterId,
    pub repetition: u32,
}

impl EpisodeIdentity {
    pub fn key(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.scenario_id,
            self.profile.as_str(),
            self.adapter.as_str(),
            self.repetition
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixSpec {
    pub identities: Vec<EpisodeIdentity>,
    pub family_counts: Vec<(String, u64)>,
    pub profile_counts: Vec<(String, u64)>,
    pub adapter_counts: Vec<(String, u64)>,
    pub repetition_counts: Vec<(u32, u64)>,
}

pub fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

pub fn episode_seed(
    campaign_seed: u64,
    scenario_seed: u64,
    profile: ProfileId,
    adapter: AdapterId,
    repetition: u32,
) -> u64 {
    let mut x = campaign_seed
        ^ scenario_seed.wrapping_mul(0xD1B5_4A32_D192_ED03)
        ^ (profile as u64).wrapping_shl(8)
        ^ (adapter as u64).wrapping_shl(16)
        ^ u64::from(repetition);
    x = splitmix64(x);
    splitmix64(x)
}

fn shuffle<T>(items: &mut [T], seed: u64) {
    let mut s = seed;
    for i in (1..items.len()).rev() {
        s = splitmix64(s);
        let j = (s as usize) % (i + 1);
        items.swap(i, j);
    }
}

/// Cartesian product in catalog order, then a seed-derived permutation.
/// Order is contract-significant. Coverage is independent of the permutation.
pub fn expected_matrix(items: &[Scenario], repeats: u32, seed: u64) -> EvalResult<MatrixSpec> {
    validate_repeats(repeats)?;
    if items.is_empty() {
        return Err(EvalError::Schema("catalog is empty".into()));
    }
    let mut identities = Vec::new();
    for scenario in items {
        for profile in &scenario.profiles {
            for adapter in &scenario.adapters {
                for repetition in 0..repeats {
                    identities.push(EpisodeIdentity {
                        scenario_id: scenario.id.clone(),
                        family: scenario.family,
                        profile: *profile,
                        adapter: *adapter,
                        repetition,
                    });
                }
            }
        }
    }
    let max_cells = items
        .len()
        .saturating_mul(ProfileId::ALL.len())
        .saturating_mul(AdapterId::ALL.len())
        .saturating_mul(MAX_REPEATS as usize);
    if identities.len() > max_cells {
        return Err(EvalError::Schema(
            "matrix exceeds finite campaign bound".into(),
        ));
    }
    shuffle(&mut identities, seed);
    Ok(summarize(identities))
}

fn summarize(identities: Vec<EpisodeIdentity>) -> MatrixSpec {
    let mut family_map = std::collections::BTreeMap::new();
    let mut profile_map = std::collections::BTreeMap::new();
    let mut adapter_map = std::collections::BTreeMap::new();
    let mut repetition_map = std::collections::BTreeMap::new();
    for id in &identities {
        *family_map
            .entry(id.family.as_str().to_string())
            .or_insert(0) += 1;
        *profile_map
            .entry(id.profile.as_str().to_string())
            .or_insert(0) += 1;
        *adapter_map
            .entry(id.adapter.as_str().to_string())
            .or_insert(0) += 1;
        *repetition_map.entry(id.repetition).or_insert(0) += 1;
    }
    MatrixSpec {
        identities,
        family_counts: family_map.into_iter().collect(),
        profile_counts: profile_map.into_iter().collect(),
        adapter_counts: adapter_map.into_iter().collect(),
        repetition_counts: repetition_map.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog;

    #[test]
    fn different_seeds_permute_schedule_but_preserve_coverage() {
        let items = catalog();
        let a = expected_matrix(&items, 2, 435_272).unwrap();
        let b = expected_matrix(&items, 2, 435_273).unwrap();
        assert_eq!(a.identities.len(), b.identities.len());
        assert_ne!(a.identities, b.identities);
        let mut sa = a.identities.clone();
        let mut sb = b.identities.clone();
        sa.sort();
        sb.sort();
        assert_eq!(sa, sb);
        assert_eq!(a.family_counts, b.family_counts);
        assert_eq!(a.profile_counts, b.profile_counts);
        assert_eq!(a.adapter_counts, b.adapter_counts);
    }

    #[test]
    fn repeats_zero_is_rejected_before_work() {
        let items = catalog();
        assert!(expected_matrix(&items, 0, 1).is_err());
    }
}
