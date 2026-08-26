//! Hermetic self-host rehearsal: admit, dispatch, hash, cancel, uncertain.
//!
//! This is not live Grok Build qualification. It uses the production fence
//! against a loopback fake coding-worker.

use std::time::Duration;

use grokptah_agent_bridge::orchestration::{
    physical_launch, unsigned_provider_spec, verify_artifact_bytes, DurableAdmission,
    ExecutionLifecycle, FakeCodingWorker, LiveRevisions, MacKey, PhysicalArtifactClaim,
    PhysicalLaunchRequest, SpineError, SpinePersist, Supervisor,
};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn key() -> MacKey {
    MacKey::from_bytes(&[0x42; 32]).unwrap()
}

#[tokio::test]
async fn hermetic_self_host_rehearsal_lifecycle() {
    let dir = tempdir().unwrap();
    let persist = SpinePersist::open(dir.path()).unwrap();
    let admission = DurableAdmission::new(persist);
    let spec = unsigned_provider_spec("dogfood", "add diagnostic assertion")
        .seal(&key())
        .unwrap();
    let admitted = admission
        .admit(
            &key(),
            spec.clone(),
            LiveRevisions::default(),
            b"add diagnostic assertion",
            1,
        )
        .unwrap();
    assert_eq!(admitted.lifecycle, ExecutionLifecycle::Queued);
    let projection = admitted.project_public().unwrap();
    projection.validate().unwrap();
    assert!(!serde_json::to_string(&projection)
        .unwrap()
        .contains("/Users/"));

    admission
        .cancel_before_send(&admitted.verified.spec().run_id, admitted.revision)
        .unwrap();
    assert_eq!(
        admission.begin_send(&spec.provider_request_id, 0),
        Err(SpineError::TransitionForbidden)
    );

    let spec2 = unsigned_provider_spec("dogfood2", "add diagnostic assertion")
        .seal(&key())
        .unwrap();
    let admitted2 = admission
        .admit(
            &key(),
            spec2.clone(),
            LiveRevisions::default(),
            b"add diagnostic assertion",
            2,
        )
        .unwrap();
    admission
        .persist_starting(&admitted2.verified.spec().run_id, admitted2.revision)
        .unwrap();
    let supervisor = Supervisor::new(1);
    let reg = supervisor
        .register_closed(
            admitted2.verified.spec().attempt_id.clone(),
            tokio::spawn(async {}),
            tokio::spawn(async {}),
            tokio::spawn(async {}),
            CancellationToken::new(),
        )
        .unwrap();
    reg.open_gate().unwrap();
    admission
        .persist_running(&admitted2.verified.spec().run_id, admitted2.revision + 1)
        .unwrap();

    let fake = FakeCodingWorker::spawn().await;
    let request = PhysicalLaunchRequest {
        provider_request_id: spec2.provider_request_id.clone(),
        workspace_id: spec2.workspace_id.clone(),
        source_revision: spec2.workspace_source_revision.clone(),
        model: spec2.model.clone(),
        objective_digest: spec2.objective_digest.clone(),
    };
    let ack = physical_launch(
        &admission,
        &fake.base_url(),
        request.clone(),
        Duration::from_secs(2),
    )
    .await
    .unwrap();
    assert_eq!(ack.provider_request_id, spec2.provider_request_id);
    assert_eq!(fake.launches().len(), 1);

    let claim = PhysicalArtifactClaim {
        artifact_id: format!("art-{}", spec2.provider_request_id),
        claimed_sha256: {
            use sha2::{Digest, Sha256};
            Sha256::digest(b"diff --git a/x b/x\n")
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        },
        path: format!("/v1/artifacts/{}", spec2.provider_request_id),
    };
    let verified = verify_artifact_bytes(&fake.base_url(), &claim, 4096)
        .await
        .unwrap();
    assert_eq!(verified.digest_sha256, claim.claimed_sha256);

    assert_eq!(
        physical_launch(
            &admission,
            &fake.base_url(),
            request,
            Duration::from_secs(2)
        )
        .await
        .unwrap_err(),
        SpineError::AutoRetryForbidden
    );
    assert_eq!(fake.launches().len(), 1);

    let spec3 = unsigned_provider_spec("dogfood3", "hang")
        .seal(&key())
        .unwrap();
    let admitted3 = admission
        .admit(&key(), spec3.clone(), LiveRevisions::default(), b"hang", 3)
        .unwrap();
    admission
        .persist_starting(&admitted3.verified.spec().run_id, admitted3.revision)
        .unwrap();
    fake.hang_next();
    let hang_req = PhysicalLaunchRequest {
        provider_request_id: spec3.provider_request_id.clone(),
        workspace_id: spec3.workspace_id,
        source_revision: spec3.workspace_source_revision,
        model: spec3.model,
        objective_digest: spec3.objective_digest,
    };
    assert_eq!(
        physical_launch(
            &admission,
            &fake.base_url(),
            hang_req,
            Duration::from_millis(80)
        )
        .await
        .unwrap_err(),
        SpineError::AutoRetryForbidden
    );
    assert_eq!(fake.launches().len(), 2);

    drop(reg);
}

#[test]
fn public_authority_schema_file_exists_and_fixture_keys_match() {
    let schema = include_str!("../../../../docs/schemas/grokptah-authority.v1.schema.json");
    let value: serde_json::Value = serde_json::from_str(schema).unwrap();
    assert_eq!(value["$id"], "urn:grokptah:schema:authority:v1");
    let fixture = grokptah_agent_sdk::authority::public_authority_fixture(
        grokptah_agent_sdk::authority::PublicGrantClass::ProviderRun,
    );
    let encoded = serde_json::to_value(&fixture).unwrap();
    for key in value["required"].as_array().unwrap() {
        assert!(
            encoded.get(key.as_str().unwrap()).is_some(),
            "fixture missing {}",
            key
        );
    }
}
