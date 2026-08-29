//! Sampler-specific helpers for the shared-HTTP-client integration binaries:
//! config + request drivers for real `SamplingClient`s. The generic
//! connection-counting server lives in `xai_grok_test_support`.

use ed25519_dalek::{Signer, SigningKey};
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

use xai_grok_sampler::{SamplerConfig, SamplingClient};
use xai_grok_sampling_types::{ContentPart, ConversationItem, ConversationRequest, UserItem};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthorityPayload {
    principal_incarnation: String,
    auth_generation: u64,
    capability_generation: u64,
    effect_lease_id: String,
    effect_scope: String,
    revoked_effect_lease_ids: Vec<String>,
    issued_effect_lease_ids: Vec<String>,
}

#[derive(Serialize)]
struct SignedAuthorityRecord {
    #[serde(flatten)]
    payload: AuthorityPayload,
    signature: String,
}

pub fn test_provider_attempt_context() -> xai_provider_attempt::AttemptContext {
    let scope = format!("test-scope-{}", Uuid::new_v4());
    let root = std::env::temp_dir().join(format!("grokptah-sampler-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(root.join("canonical-authorities")).unwrap();
    let signing_key = SigningKey::from_bytes(&[7; 32]);
    let public_key = root
        .join("canonical-authorities")
        .join(".authority-public-key");
    std::fs::write(&public_key, signing_key.verifying_key().to_bytes()).unwrap();
    let lease_id = format!("test-lease-{}", Uuid::new_v4());
    let payload = AuthorityPayload {
        principal_incarnation: "test-principal".into(),
        auth_generation: 1,
        capability_generation: 1,
        effect_lease_id: lease_id.clone(),
        effect_scope: scope.clone(),
        revoked_effect_lease_ids: Vec::new(),
        issued_effect_lease_ids: {
            let mut leases = vec![lease_id];
            leases.extend((1..64).map(|_| format!("test-lease-{}", Uuid::new_v4())));
            leases
        },
    };
    let signature = signing_key.sign(&serde_json::to_vec(&payload).unwrap());
    std::fs::write(
        root.join("canonical-authorities")
            .join(format!("{scope}.json")),
        serde_json::to_vec(&SignedAuthorityRecord {
            payload,
            signature: signature
                .to_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        })
        .unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&public_key, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(
            root.join("canonical-authorities")
                .join(format!("{scope}.json")),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let store = xai_provider_attempt::ProviderAttemptStore::open(root).unwrap();
    xai_provider_attempt::AttemptContext::from_host_ledger(
        store,
        format!("test-operation-{}", Uuid::new_v4()),
        scope,
    )
    .unwrap()
}

#[allow(dead_code)]
pub fn test_config(base_url: &str, api_key: &str) -> SamplerConfig {
    let mut config = SamplerConfig {
        api_key: Some(api_key.to_string()),
        base_url: base_url.to_string(),
        model: "test-model".to_string(),
        ..SamplerConfig::default()
    };
    config.provider_attempt = Some(test_provider_attempt_context());
    config
}

/// Drive one POST through the client; the canned `{}` body is not a valid
/// completion, but only the wire-level request matters here.
#[allow(dead_code)]
pub async fn send_one(client: &SamplingClient) {
    let request = ConversationRequest {
        items: vec![ConversationItem::User(UserItem {
            content: vec![ContentPart::Text {
                text: Arc::<str>::from("hi"),
            }],
            ..Default::default()
        })],
        ..Default::default()
    };
    let _ = client.conversation(request).await;
}
