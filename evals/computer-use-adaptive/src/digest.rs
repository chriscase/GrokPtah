//! Canonical fixture and campaign identity. Labels alone are not hashed.

use sha2::{Digest, Sha256};

use crate::catalog::Scenario;
use crate::naming::NamingRecord;
use crate::schema::to_canonical_json;
use crate::types::{EvalError, EvalResult};

pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

/// Digest of one scenario covering every behavior-defining field.
pub fn scenario_digest(scenario: &Scenario) -> EvalResult<String> {
    let json = to_canonical_json(scenario)?;
    Ok(sha256_hex(json.as_bytes()))
}

/// Catalog identity. Changing world, scripts, crash cuts, expected cells,
/// profiles, adapters, held-out flags, or ids changes this digest.
pub fn fixture_hash(items: &[Scenario]) -> EvalResult<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah.cu_eval_fixture.v1\0");
    hasher.update((items.len() as u64).to_le_bytes());
    for scenario in items {
        let digest = scenario_digest(scenario)?;
        hasher.update(digest.as_bytes());
        hasher.update(b"\n");
    }
    Ok(hex_encode(&hasher.finalize()))
}

/// Campaign identity: fixtures + repeats + seed + naming + matrix size.
#[allow(clippy::too_many_arguments)]
pub fn campaign_digest(
    fixture: &str,
    repeats: u32,
    seed: u64,
    episode_count: u64,
    naming: &NamingRecord,
    episode_digests: &[String],
    evidence_digests: &[String],
    source_head: &str,
    source_tree: &str,
    source_base: &str,
) -> EvalResult<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah.cu_eval_campaign.v1\0");
    hasher.update(fixture.as_bytes());
    hasher.update(repeats.to_le_bytes());
    hasher.update(seed.to_le_bytes());
    hasher.update(episode_count.to_le_bytes());
    let naming_json = to_canonical_json(naming)?;
    hasher.update(naming_json.as_bytes());
    hasher.update(source_head.as_bytes());
    hasher.update(source_tree.as_bytes());
    hasher.update(source_base.as_bytes());
    hasher.update((episode_digests.len() as u64).to_le_bytes());
    for digest in episode_digests {
        hasher.update(digest.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update((evidence_digests.len() as u64).to_le_bytes());
    for digest in evidence_digests {
        hasher.update(digest.as_bytes());
        hasher.update(b"\0");
    }
    Ok(hex_encode(&hasher.finalize()))
}

pub fn evidence_content_digest<T: serde::Serialize>(value: &T) -> EvalResult<String> {
    let json = to_canonical_json(value)?;
    Ok(sha256_hex(json.as_bytes()))
}

/// Digest of an evidence object excluding the `contentSha256` field itself.
pub fn evidence_body_digest<T: serde::Serialize>(value: &T) -> EvalResult<String> {
    let raw = serde_json::to_value(value).map_err(|e| EvalError::Schema(e.to_string()))?;
    let mut stripped = raw;
    if let Some(obj) = stripped.as_object_mut() {
        obj.remove("contentSha256");
    }
    Ok(sha256_hex(
        crate::schema::canonical_value(&stripped).as_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog;

    #[test]
    fn changing_world_changes_fixture_digest() {
        let items = catalog();
        let a = fixture_hash(&items).unwrap();
        let mut b_items = items.clone();
        b_items[0].world.success_flag = "mutated_flag".into();
        let b = fixture_hash(&b_items).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn changing_script_or_expected_changes_digest() {
        let items = catalog();
        let a = fixture_hash(&items).unwrap();
        let mut script = items.clone();
        if let Some(ev) = script[0].script.first_mut() {
            ev.at_step = ev.at_step.saturating_add(3);
        } else {
            script[0].objective.push_str(" mutated");
        }
        assert_ne!(a, fixture_hash(&script).unwrap());
        let mut expected = items.clone();
        expected[0].expected.cells[0].task_success = !expected[0].expected.cells[0].task_success;
        assert_ne!(a, fixture_hash(&expected).unwrap());
    }

    #[test]
    fn documented_handoff_hashes_match_reconstructed_catalog() {
        let items = catalog();
        let fixture = fixture_hash(&items).unwrap();
        assert_eq!(
            fixture,
            "614a8b4b0bf5d5f559764f894661475a11e75e1e40279bdbe5e48cf5387cc20a"
        );
        let naming = crate::naming::NamingRecord::decision_packet();
        let digest = campaign_digest(
            &fixture,
            5,
            435_272,
            2100,
            &naming,
            &[],
            &[],
            "head",
            "tree",
            "base",
        )
        .unwrap();
        let other = campaign_digest(
            &fixture,
            5,
            435_273,
            2100,
            &naming,
            &[],
            &[],
            "head",
            "tree",
            "base",
        )
        .unwrap();
        assert_ne!(digest, other);
    }
}
