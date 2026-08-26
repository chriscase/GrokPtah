//! Adversarial source proof for the reconstructed authority spine.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use grokptah_agent_bridge::orchestration::{
    canonical_mac_bytes, derive_grant, mac_over_fields, parse_bounds_json, sha256_hex,
    unsigned_provider_spec, verify_fields, DurableAdmission, ExecutionLifecycle, HostGrant,
    InternalExecutionSpec, LiveRevisions, MacKey, ProviderSendState, Revision, SendCutTable,
    SendRecovery, SpineError, SpinePersist, Supervisor, MAC_DOMAIN_SPEC,
};
use grokptah_agent_sdk::authority::{
    public_authority_fixture, PublicAuthorityProjection, PublicGrantClass,
    PUBLIC_AUTHORITY_CONTRACT_VERSION,
};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn test_key() -> MacKey {
    MacKey::from_bytes(&[0x4b; 32]).expect("32-byte key")
}

fn seal(suffix: &str) -> InternalExecutionSpec {
    unsigned_provider_spec(suffix, "intent")
        .seal(&test_key())
        .unwrap()
}

#[test]
fn mac_domain_key_order_and_length_substitution_fail_closed() {
    let key = test_key();
    let other = MacKey::from_bytes(&[0x5a; 32]).unwrap();
    let fields_a: &[(&str, &[u8])] = &[("left", b"ab"), ("right", b"c")];
    let fields_b: &[(&str, &[u8])] = &[("left", b"a"), ("right", b"bc")];
    let tag = mac_over_fields(&key, MAC_DOMAIN_SPEC, fields_a).unwrap();
    assert_ne!(
        tag,
        mac_over_fields(&key, MAC_DOMAIN_SPEC, fields_b).unwrap()
    );
    assert_ne!(
        tag,
        mac_over_fields(&key, MAC_DOMAIN_SPEC, &[("right", b"c"), ("left", b"ab")]).unwrap()
    );
    assert_ne!(
        tag,
        mac_over_fields(&key, "grokptah.authority.other.v1", fields_a).unwrap()
    );
    assert_ne!(
        tag,
        mac_over_fields(&other, MAC_DOMAIN_SPEC, fields_a).unwrap()
    );
    verify_fields(&key, MAC_DOMAIN_SPEC, fields_a, &tag).unwrap();
    assert_eq!(
        verify_fields(&key, MAC_DOMAIN_SPEC, fields_b, &tag),
        Err(SpineError::MacInvalid)
    );
    let canonical = canonical_mac_bytes(MAC_DOMAIN_SPEC, fields_a).unwrap();
    assert!(canonical.starts_with(b"GPTA.MAC.v1"));
}

#[test]
fn coordinated_identity_substitution_is_rejected() {
    let key = test_key();
    let sealed = seal("chain");
    sealed.verify(&key).unwrap();
    let mut swapped = sealed.clone();
    std::mem::swap(&mut swapped.request_id, &mut swapped.run_id);
    assert_eq!(swapped.verify(&key), Err(SpineError::MacInvalid));
    let mut duplicated = unsigned_provider_spec("dup", "intent");
    duplicated.run_id = duplicated.request_id.clone();
    assert_eq!(
        duplicated.seal(&key).unwrap_err(),
        SpineError::DuplicateIdentity
    );
}

#[test]
fn identical_payload_across_scopes_has_distinct_macs() {
    let key = test_key();
    let a = unsigned_provider_spec("a", "same-payload")
        .seal(&key)
        .unwrap();
    let mut b = unsigned_provider_spec("b", "same-payload");
    b.tenant = "tenant-2".into();
    b.project = "project-2".into();
    b.workspace_id = "workspace-2".into();
    b.session = "session-2".into();
    let b = b.seal(&key).unwrap();
    assert_ne!(a.spec_mac_hex, b.spec_mac_hex);
}

#[test]
fn agent_effort_revisions_bounds_and_digests_are_covered() {
    let key = test_key();
    let sealed = seal("cover");
    for mutate in [
        |spec: &mut InternalExecutionSpec| spec.agent = "agent-x".into(),
        |spec: &mut InternalExecutionSpec| spec.effort = "low".into(),
        |spec: &mut InternalExecutionSpec| spec.policy_revision = Revision::new(9),
        |spec: &mut InternalExecutionSpec| spec.capability_revision = Revision::new(9),
        |spec: &mut InternalExecutionSpec| spec.credential_revision = Revision::new(9),
        |spec: &mut InternalExecutionSpec| spec.route_revision = Revision::new(9),
        |spec: &mut InternalExecutionSpec| spec.source_revision = Revision::new(9),
        |spec: &mut InternalExecutionSpec| spec.bounds.max_rounds = 99,
        |spec: &mut InternalExecutionSpec| spec.attempt_ordinal = 8,
        |spec: &mut InternalExecutionSpec| spec.lease_epoch = 8,
        |spec: &mut InternalExecutionSpec| spec.input_digest = sha256_hex(b"other"),
        |spec: &mut InternalExecutionSpec| spec.provider_request_id = "preq-x".into(),
    ] {
        let mut mutated = sealed.clone();
        mutate(&mut mutated);
        assert_eq!(
            mutated.verify(&key),
            Err(SpineError::MacInvalid),
            "expected MAC failure after mutation"
        );
    }
}

#[test]
fn revision_revoke_and_overflow_fail_closed() {
    let dir = tempdir().unwrap();
    let admission = DurableAdmission::new(SpinePersist::open(dir.path()).unwrap());
    let key = test_key();
    let sealed = seal("rev");
    let revoked = LiveRevisions {
        credential: Revision::new(2),
        ..LiveRevisions::default()
    };
    assert_eq!(
        admission
            .admit(&key, sealed, revoked, b"intent", 1)
            .unwrap_err(),
        SpineError::StaleRevision
    );
    assert_eq!(
        Revision::new(u64::MAX).checked_next(),
        Err(SpineError::RevisionOverflow)
    );
}

#[test]
fn bounds_omission_unknown_keys_and_utf8_ceilings() {
    assert_eq!(
        parse_bounds_json("{}").unwrap_err(),
        SpineError::UnknownField
    );
    assert_eq!(
        parse_bounds_json("{\"maxPromptBytes\":1,\"maxRounds\":1,\"maxDurationMs\":1,\"extra\":1}")
            .unwrap_err(),
        SpineError::UnknownField
    );
    assert!(
        parse_bounds_json("{\"maxPromptBytes\":32,\"maxRounds\":1,\"maxDurationMs\":1}").is_ok()
    );
    assert_eq!(
        parse_bounds_json("{\"maxPromptBytes\":0,\"maxRounds\":1,\"maxDurationMs\":1}")
            .unwrap()
            .validate(),
        Err(SpineError::BoundsOmitted)
    );
    let key = test_key();
    let mut spec = unsigned_provider_spec("utf8", "ok");
    spec.principal = "x".repeat(5000);
    assert_eq!(spec.seal(&key).unwrap_err(), SpineError::Utf8Ceiling);
}

#[test]
fn every_modeled_send_crash_cut() {
    let mut table = SendCutTable::default();
    table.prepare().unwrap();
    assert_eq!(
        table.recover().unwrap(),
        SendRecovery::AutoRetryKnownNotSent
    );
    table.step(ProviderSendState::Sending).unwrap();
    assert_eq!(table.recover().unwrap(), SendRecovery::UncertainNoRetry);
    table.step(ProviderSendState::Uncertain).unwrap();
    assert_eq!(table.recover().unwrap(), SendRecovery::UncertainNoRetry);
    assert_eq!(
        table.step(ProviderSendState::KnownNotSent).unwrap_err(),
        SpineError::TransitionForbidden
    );

    let mut sent = SendCutTable::default();
    sent.prepare().unwrap();
    sent.step(ProviderSendState::Sending).unwrap();
    sent.step(ProviderSendState::Sent).unwrap();
    sent.step(ProviderSendState::Streaming).unwrap();
    sent.step(ProviderSendState::Completed).unwrap();
    assert_eq!(sent.recover().unwrap(), SendRecovery::AlreadySettled);
}

#[test]
fn durable_cancel_before_send_and_duplicate_replay() {
    let dir = tempdir().unwrap();
    let admission = DurableAdmission::new(SpinePersist::open(dir.path()).unwrap());
    let key = test_key();
    let spec = seal("c0");
    let admitted = admission
        .admit(&key, spec.clone(), LiveRevisions::default(), b"intent", 1)
        .unwrap();
    assert_eq!(admitted.lifecycle, ExecutionLifecycle::Queued);
    let replay = admission
        .replay_or_admit(&key, spec.clone(), LiveRevisions::default(), b"intent", 2)
        .unwrap();
    assert_eq!(
        replay.verified.spec().run_id,
        admitted.verified.spec().run_id
    );
    admission
        .cancel_before_send(&admitted.verified.spec().run_id, admitted.revision)
        .unwrap();
    assert_eq!(
        admission.begin_send(&admitted.verified.spec().provider_request_id, 0),
        Err(SpineError::TransitionForbidden)
    );
}

#[test]
fn cross_tenant_replay_is_rejected() {
    let dir = tempdir().unwrap();
    let admission = DurableAdmission::new(SpinePersist::open(dir.path()).unwrap());
    let key = test_key();
    let spec = seal("xt");
    admission
        .admit(&key, spec.clone(), LiveRevisions::default(), b"intent", 1)
        .unwrap();
    let mut other = spec;
    other.tenant = "tenant-other".into();
    assert_eq!(
        admission
            .replay_or_admit(&key, other, LiveRevisions::default(), b"intent", 2)
            .unwrap_err(),
        SpineError::CrossScope
    );
}

#[tokio::test]
async fn completion_racing_cancel_and_drop() {
    let supervisor = Supervisor::new(1);
    let id;
    {
        let reg = supervisor
            .register_closed(
                "worker-1",
                tokio::spawn(async {}),
                tokio::spawn(async {}),
                tokio::spawn(async {}),
                CancellationToken::new(),
            )
            .unwrap();
        id = reg.id().to_string();
        assert_eq!(supervisor.used(), 1);
        drop(reg);
    }
    let (cleanup, released, death) = supervisor.slot_flags(&id).unwrap();
    assert!(cleanup);
    assert!(!released);
    assert!(!death);
    assert_eq!(supervisor.used(), 1);
    supervisor.abort_id(&id).unwrap();
    supervisor.release_capacity(&id).unwrap();
    assert_eq!(supervisor.used(), 0);
}

#[test]
fn nested_redaction_needles_never_project() {
    let sealed = seal("redact");
    let projection = sealed
        .project_public(
            grokptah_agent_sdk::authority::PublicExecutionLifecycle::Queued,
            grokptah_agent_sdk::authority::PublicSendState::KnownNotSent,
        )
        .unwrap();
    let encoded = serde_json::to_string(&projection).unwrap();
    for needle in [
        "hmac",
        "mac_key",
        "/Users/",
        "api_key",
        "Bearer ",
        "credential_ref",
        &sealed.spec_mac_hex,
        "intent",
    ] {
        assert!(!encoded.contains(needle), "leaked {needle}: {encoded}");
    }
    public_authority_fixture(PublicGrantClass::HelpAnswer)
        .validate()
        .unwrap();
}

#[test]
fn deterministic_public_fixtures_and_unknown_fields() {
    let fixture = public_authority_fixture(PublicGrantClass::ProviderRun);
    assert_eq!(fixture.contract, PUBLIC_AUTHORITY_CONTRACT_VERSION);
    let mut extra = serde_json::to_value(&fixture).unwrap();
    extra["grantConstructor"] = serde_json::json!("nope");
    assert!(serde_json::from_value::<PublicAuthorityProjection>(extra).is_err());
}

#[test]
fn derive_grant_from_verified_spec_only() {
    let key = test_key();
    let sealed = seal("grant");
    let verified = sealed.verify(&key).unwrap();
    assert!(matches!(
        derive_grant(&verified).unwrap(),
        HostGrant::ProviderRun { .. }
    ));
}

#[test]
fn only_known_not_sent_may_auto_retry() {
    assert!(ProviderSendState::KnownNotSent.may_auto_retry());
    assert!(!ProviderSendState::Sending.may_auto_retry());
    assert!(!ProviderSendState::Uncertain.may_auto_retry());
    assert!(!ProviderSendState::Sent.may_auto_retry());
}

#[test]
fn negative_external_consumer_compile_gate() {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sdk = crate_dir.join("../../common/grokptah-agent-sdk");
    let bridge = crate_dir.clone();
    let tmp = tempdir().unwrap();
    let consumer = tmp.path().join("negative-consumer");
    fs::create_dir_all(consumer.join("src")).unwrap();
    fs::write(
        consumer.join("Cargo.toml"),
        format!(
            r#"[package]
name = "authority-spine-negative-consumer"
version = "0.0.0"
edition = "2021"
[dependencies]
grokptah-agent-sdk = {{ path = "{}" }}
grokptah-agent-bridge = {{ path = "{}" }}
"#,
            sdk.display(),
            bridge.display()
        ),
    )
    .unwrap();
    fs::write(
        consumer.join("src/lib.rs"),
        r#"
pub fn cannot_construct_mac_key() {
    let _ = grokptah_agent_sdk::authority::MacKey::from_bytes(&[0u8; 32]);
}
pub fn cannot_sign_intent() {
    grokptah_agent_sdk::authority::sign_intent(b"x");
}
pub fn cannot_admit_public_projection() {
    let projection = grokptah_agent_sdk::authority::public_authority_fixture(
        grokptah_agent_sdk::authority::PublicGrantClass::HelpAnswer,
    );
    let _ = grokptah_agent_bridge::orchestration::admit_verified_only(
        &grokptah_agent_bridge::orchestration::MacKey::from_bytes(&[0u8; 32]).unwrap(),
        projection,
        grokptah_agent_bridge::orchestration::LiveRevisions::default(),
    );
}
"#,
    )
    .unwrap();
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("authority-spine-negative-target"));
    let nested = target.join("negative-consumer");
    fs::create_dir_all(&nested).unwrap();
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&consumer)
        .arg("check")
        .arg("--offline")
        .arg("--quiet")
        .env("CARGO_TARGET_DIR", &nested);
    if let Ok(wrapper) = std::env::var("RUSTC_WRAPPER") {
        cmd.env("RUSTC_WRAPPER", wrapper);
    }
    if let Ok(dir) = std::env::var("SCCACHE_DIR") {
        cmd.env("SCCACHE_DIR", dir);
    }
    if let Ok(port) = std::env::var("SCCACHE_SERVER_PORT") {
        cmd.env("SCCACHE_SERVER_PORT", port);
    }
    let output = cmd
        .output()
        .expect("spawn cargo check for negative consumer");
    assert!(
        !output.status.success(),
        "negative consumer compiled; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("MacKey")
            || stderr.contains("sign_intent")
            || stderr.contains("AuthenticatedEnvelope")
            || stderr.contains("expected InternalExecutionSpec")
            || stderr.contains("E0433")
            || stderr.contains("E0425")
            || stderr.contains("E0308"),
        "missing expected diagnostic: {stderr}"
    );
}
