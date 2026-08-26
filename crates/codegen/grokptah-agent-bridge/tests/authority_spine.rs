//! Host authority envelope: MAC coverage, typestate, exact identities.

use grokptah_agent_bridge::orchestration::{
    AuthorityKey, ClassifiedId, ExecutionBounds, HostGrantClass, IdentityClass, UnverifiedEnvelope,
};

fn key() -> AuthorityKey {
    AuthorityKey::provision("test-key", 1, &[0x11; 32]).unwrap()
}

fn provider_run_ids() -> Vec<ClassifiedId> {
    vec![
        ClassifiedId::new(IdentityClass::Request, "req-1").unwrap(),
        ClassifiedId::new(IdentityClass::Work, "work-1").unwrap(),
        ClassifiedId::new(IdentityClass::Run, "run-1").unwrap(),
        ClassifiedId::new(IdentityClass::Attempt, "att-1").unwrap(),
        ClassifiedId::new(IdentityClass::Lease, "lease-1").unwrap(),
        ClassifiedId::new(IdentityClass::ProviderRequest, "prq-1").unwrap(),
    ]
}

fn unsigned(identities: Vec<ClassifiedId>) -> UnverifiedEnvelope {
    UnverifiedEnvelope {
        principal: "principal-1".into(),
        tenant: "tenant-1".into(),
        project: "project-1".into(),
        workspace: "workspace-1".into(),
        session: "session-1".into(),
        agent: Some("agent-1".into()),
        provider: "xai".into(),
        profile: "xai".into(),
        endpoint_fingerprint: "ep-fp-1".into(),
        model: "grok-4".into(),
        effort: Some("high".into()),
        auth_revision: 1,
        policy_revision: 2,
        capability_revision: 3,
        credential_revision: 4,
        source_revision: 5,
        bounds: ExecutionBounds {
            max_duration_ms: 60_000,
            max_rounds: 8,
            max_tokens: 32_000,
            max_cost_cents: 50,
            max_tools: 16,
        },
        identities,
        grant_class: HostGrantClass::ProviderRun,
        intent_digest: "sha256:abc".into(),
        expires_unix: 1_900_000_000,
        key_id: String::new(),
        key_version: 0,
        envelope_mac_hex: String::new(),
    }
}

#[test]
fn deserialized_bytes_are_unverified_until_host_mac_check() {
    let key = key();
    let verified = unsigned(provider_run_ids()).seal(&key).unwrap();
    let json = serde_json::to_string(verified.inner()).unwrap();
    let unverified: UnverifiedEnvelope = serde_json::from_str(&json).unwrap();
    unverified
        .verify(&key)
        .expect("host verification must accept its own envelope");
}

#[test]
fn extra_identity_is_rejected() {
    let mut ids = provider_run_ids();
    ids.push(ClassifiedId::new(IdentityClass::Tombstone, "tomb-1").unwrap());
    assert!(unsigned(ids).seal(&key()).is_err());
}

#[test]
fn flipping_agent_effort_or_a_revision_invalidates_the_mac() {
    let key = key();
    let verified = unsigned(provider_run_ids()).seal(&key).unwrap();
    for mutate in [
        |e: &mut UnverifiedEnvelope| e.agent = Some("agent-2".into()),
        |e: &mut UnverifiedEnvelope| e.effort = Some("low".into()),
        |e: &mut UnverifiedEnvelope| e.auth_revision = 9,
        |e: &mut UnverifiedEnvelope| e.policy_revision = 9,
        |e: &mut UnverifiedEnvelope| e.capability_revision = 9,
        |e: &mut UnverifiedEnvelope| e.credential_revision = 9,
        |e: &mut UnverifiedEnvelope| e.source_revision = 9,
        |e: &mut UnverifiedEnvelope| e.bounds.max_tokens = 1,
    ] {
        let mut raw = verified.inner().clone();
        mutate(&mut raw);
        assert!(raw.verify(&key).is_err());
    }
}
