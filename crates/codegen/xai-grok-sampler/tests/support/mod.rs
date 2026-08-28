//! Sampler-specific helpers for the shared-HTTP-client integration binaries:
//! config + request drivers for real `SamplingClient`s. The generic
//! connection-counting server lives in `xai_grok_test_support`.

use std::sync::Arc;
use uuid::Uuid;

use xai_grok_sampler::{SamplerConfig, SamplingClient};
use xai_grok_sampling_types::{ContentPart, ConversationItem, ConversationRequest, UserItem};

pub fn test_provider_attempt_context() -> xai_provider_attempt::AttemptContext {
    let scope = format!("test-scope-{}", Uuid::new_v4());
    let root = std::env::temp_dir().join(format!("grokptah-sampler-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(root.join("canonical-authorities")).unwrap();
    std::fs::write(
        root.join("canonical-authorities")
            .join(format!("{scope}.json")),
        serde_json::json!({
            "principalIncarnation": "test-principal",
            "authGeneration": 1,
            "capabilityGeneration": 1,
            "effectLeaseId": format!("test-lease-{}", Uuid::new_v4()),
            "effectScope": scope,
            "revokedEffectLeaseIds": [],
        })
        .to_string(),
    )
    .unwrap();
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
