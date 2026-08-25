//! Fixture-only certification surface for the v2 durable memory core.
//!
//! This integration test consumes the checked-in deny-unknown fixture and
//! does not self-authorize storage through [`MemoryScope`]. Host-owned writes
//! remain on `MemoryAccess` inside the crate. There are no live provider
//! calls, sleeps, or Tokio-time claims.

use grokptah_agent_bridge::{MemoryLongHorizonEvidence, MemoryScope};
use serde::Deserialize;
use serde_json::Value;

const FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../evals/certification-lab/replay-fixtures/memory-long-horizon.v1.json"
));
const EVIDENCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../docs/evidence/memory-long-horizon-campaign-v1.json"
));

/// Runtime-only canary. Must never appear in errors, debug codes, or the fixture.
const SCOPE_CANARY: &str = "gp_canary_7f3a9c2e1b_not_for_errors";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    schema: String,
    fixture_id: String,
    kind: String,
    epoch: String,
    logical_years: u32,
    logical_year_days: i64,
    seed: u64,
    scopes: FixtureScopes,
    declared_bounds: DeclaredBounds,
    workload: Workload,
    oracle: Oracle,
    product_claim: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureScopes {
    private_agent_id: String,
    team_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeclaredBounds {
    max_facts: usize,
    max_fact_chars: usize,
    max_tags: usize,
    max_tag_chars: usize,
    max_id_chars: usize,
    max_key_chars: usize,
    max_source_actor_chars: usize,
    max_query_chars: usize,
    max_hot_store_bytes: usize,
    max_receipts: usize,
    max_persisted_bytes: usize,
    max_scope_footprint_bytes: usize,
    max_scope_files: usize,
    max_critical_facts: usize,
    max_critical_bytes: usize,
    schema: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Workload {
    critical_invariants_per_scope: usize,
    min_critical_sample: usize,
    preference_revisions: u32,
    expiring_status_ttl_days: i64,
    conflict_pairs: usize,
    writes_per_year: u32,
    lexical_topk: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Oracle {
    critical_recall_pct: u32,
    stale_as_current_pct: u32,
    conflict_recall_pct: u32,
    conflict_false_positive_pct: u32,
    duplicate_rate_pct: u32,
    hot_store_within_byte_bound: bool,
    repeated_read_reopen_deterministic: bool,
}

fn consume_bounds(bounds: &DeclaredBounds) {
    assert_eq!(bounds.max_facts, 80);
    assert_eq!(bounds.max_fact_chars, 800);
    assert_eq!(bounds.max_tags, 16);
    assert_eq!(bounds.max_tag_chars, 64);
    assert_eq!(bounds.max_id_chars, 128);
    assert_eq!(bounds.max_key_chars, 128);
    assert_eq!(bounds.max_source_actor_chars, 128);
    assert_eq!(bounds.max_query_chars, 256);
    assert_eq!(bounds.max_hot_store_bytes, 128 * 1024);
    assert_eq!(bounds.max_receipts, 512);
    assert_eq!(bounds.max_persisted_bytes, 192 * 1024);
    assert_eq!(bounds.max_scope_footprint_bytes, 384 * 1024);
    assert_eq!(bounds.max_scope_files, 24);
    assert_eq!(bounds.max_critical_facts, 16);
    assert_eq!(bounds.max_critical_bytes, 16 * 1024);
    assert_eq!(bounds.schema, "grokptah.memory.v2");
}

fn consume_workload(workload: &Workload) {
    assert_eq!(workload.critical_invariants_per_scope, 3);
    assert_eq!(workload.min_critical_sample, 9);
    assert_eq!(workload.preference_revisions, 2);
    assert_eq!(workload.expiring_status_ttl_days, 30);
    assert_eq!(workload.conflict_pairs, 1);
    assert_eq!(workload.writes_per_year, 3);
    assert_eq!(workload.lexical_topk, 5);
    assert!(workload.min_critical_sample >= workload.critical_invariants_per_scope * 3);
}

fn consume_oracle(oracle: &Oracle) {
    assert_eq!(oracle.critical_recall_pct, 100);
    assert_eq!(oracle.stale_as_current_pct, 0);
    assert_eq!(oracle.conflict_recall_pct, 100);
    assert_eq!(oracle.conflict_false_positive_pct, 0);
    assert_eq!(oracle.duplicate_rate_pct, 0);
    assert!(oracle.hot_store_within_byte_bound);
    assert!(oracle.repeated_read_reopen_deterministic);
}

#[test]
fn long_horizon_fixture_is_typed_deny_unknown_and_secret_free() {
    let parsed: Value = serde_json::from_str(FIXTURE).unwrap();
    assert!(parsed.is_object());
    let fixture: Fixture = serde_json::from_value(parsed).expect("deny-unknown fixture");

    assert_eq!(fixture.schema, "grokptah.memory_long_horizon_fixture.v2");
    assert_eq!(fixture.fixture_id, "memory-long-horizon-v1");
    assert_eq!(fixture.kind, "deterministic_logical_years");
    assert_eq!(fixture.logical_years, 10);
    assert_eq!(fixture.logical_year_days, 365);
    assert_eq!(fixture.seed, 1_296_386_353);
    assert_eq!(fixture.scopes.private_agent_id, "horizon-agent");
    assert_eq!(fixture.scopes.team_id, "horizon-team");
    assert_eq!(fixture.product_claim, "safe_v2_hot_store_core");
    consume_bounds(&fixture.declared_bounds);
    consume_workload(&fixture.workload);
    consume_oracle(&fixture.oracle);

    assert_eq!(fixture.epoch, "2000-01-01T00:00:00.000Z");
    let derived_span_days = i64::from(fixture.logical_years) * fixture.logical_year_days;
    assert_eq!(derived_span_days, 3_650);

    assert!(!FIXTURE.to_ascii_lowercase().contains("api_key="));
    assert!(!FIXTURE.contains("sk-"));
    assert!(!FIXTURE.contains(SCOPE_CANARY));
    assert!(!FIXTURE.contains("password="));

    // MemoryScope remains a host DTO only. Storage, clocks, and paths are not
    // callable from this integration crate without a host-resolved address.
    let _ = MemoryScope::Project.label();
    let _ = MemoryScope::AgentPrivate {
        agent_id: fixture.scopes.private_agent_id.clone(),
    }
    .label();
    let _ = MemoryScope::Team {
        team_id: fixture.scopes.team_id.clone(),
    }
    .label();
}

#[test]
fn retained_logical_years_evidence_is_digest_bound_but_not_overclaimed() {
    let evidence: MemoryLongHorizonEvidence =
        serde_json::from_str(EVIDENCE).expect("retained memory evidence JSON");
    evidence
        .validate()
        .expect("retained memory evidence validates");
    assert!(!evidence.certification_ready());
    assert_eq!(evidence.logical_years, 10);
    assert_eq!(evidence.scopes.len(), 3);
    assert!(!EVIDENCE.to_ascii_lowercase().contains("api_key"));
    assert!(!EVIDENCE.contains("bearer"));
}
