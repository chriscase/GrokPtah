//! Task-local binding of one physical provider request to its durable attempt.
//!
//! The orchestration spawn path scopes this before `session_prompt`. The HTTP
//! client then stamps the same provider-request identity on the wire and
//! advances `Sending -> Sent -> Responding` as the request actually leaves.

use std::future::Future;

use grokptah_agent_sdk::attempt::{BoundedId, SendState};

use crate::orchestration::OrchStore;

#[derive(Clone)]
pub struct PhysicalSendBinding {
    pub store: OrchStore,
    pub attempt_id: String,
    pub provider_request_id: String,
}

tokio::task_local! {
    static PHYSICAL_SEND: PhysicalSendBinding;
}

pub async fn scope_optional<F: Future>(binding: Option<PhysicalSendBinding>, fut: F) -> F::Output {
    match binding {
        Some(binding) => PHYSICAL_SEND.scope(binding, fut).await,
        None => fut.await,
    }
}

pub fn provider_request_id() -> Option<String> {
    PHYSICAL_SEND
        .try_with(|binding| binding.provider_request_id.clone())
        .ok()
}

/// HTTP status received: the request left this host.
pub fn mark_sent() {
    advance(SendState::Sent);
}

/// Response body has begun.
pub fn mark_responding() {
    advance(SendState::Responding);
}

fn advance(next: SendState) {
    let _ = PHYSICAL_SEND.try_with(|binding| {
        let _ = binding
            .store
            .update_attempt(&binding.attempt_id, |attempt| {
                if attempt.send_state == next {
                    return Ok(());
                }
                if next == SendState::Sent && !attempt.receipts.acknowledged() {
                    attempt.receipts.request = BoundedId::new(&binding.provider_request_id);
                }
                if next == SendState::Responding {
                    attempt.receipts.provider_replied = true;
                }
                if attempt.send_state.permits_transition_to(next) {
                    attempt.advance(next).map_err(anyhow::Error::msg)?;
                }
                Ok(())
            });
    });
}
