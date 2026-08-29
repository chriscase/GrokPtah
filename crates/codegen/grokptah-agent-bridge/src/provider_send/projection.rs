//! The public projection of a provider attempt (#478).
//!
//! Everything a UI, an MCP client, or an operator report may see. Prompts,
//! bodies, credentials, raw routes and paths, private provider request ids, and
//! transport diagnostics never appear here — but the projection is not vague
//! about the thing that matters: it says plainly whether delivery is known or
//! unknown, and never rounds "unknown" down to "failed".

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::dialect::WireDialect;
use super::identity::{CallSiteFamily, SendOrigin};
use super::record::{AuditOutcome, CancellationRecord, ProviderAttempt, SettlementOutcome};
use super::state::{DeliveryKnowledge, ProviderAttemptState, UncertaintyClass};

/// Schema version of the public projection.
pub const PROVIDER_ATTEMPT_PROJECTION_VERSION: u32 = 1;

/// Redacted public view of one physical provider-send attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAttemptProjection {
    pub projection_version: u32,
    /// Short opaque handle. Not the provider's request id.
    pub attempt: String,
    /// Monotonic position within the send scope.
    pub ordinal: u64,
    pub state: ProviderAttemptState,
    /// The honest delivery answer, including "unknown".
    pub delivery: DeliveryKnowledge,
    /// Whether an automatic retry of this attempt is permitted at all.
    pub auto_retry_permitted: bool,
    pub origin: SendOrigin,
    pub family: CallSiteFamily,
    pub dialect: WireDialect,
    /// Operator-chosen model id. Public by construction.
    pub model: String,
    /// Opaque scope handles, never the workspace path or the session id.
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    pub created_at: DateTime<Utc>,
    pub state_changed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement: Option<SettlementProjection>,
}

/// Redacted public view of a settlement bundle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettlementProjection {
    pub outcome: SettlementOutcome,
    pub cancellation: CancellationRecord,
    pub audit: AuditOutcome,
    /// Whether the provider issued a receipt at all. The receipt value itself
    /// is private.
    pub provider_receipt_present: bool,
    /// Coarse HTTP class (`2xx`, `4xx`, …) rather than the exact status, which
    /// can encode gateway-specific detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    pub request_bytes: u64,
    pub response_bytes: u64,
    /// Present only when the outcome is uncertain, and coarse by design.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<UncertaintyClass>,
    pub settled_at: DateTime<Utc>,
}

fn status_class(status: u16) -> &'static str {
    match status {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    }
}

impl ProviderAttemptProjection {
    /// Project a durable record for public consumption.
    pub fn of(record: &ProviderAttempt) -> Self {
        let binding = &record.binding;
        let scope = binding.scope();
        Self {
            projection_version: PROVIDER_ATTEMPT_PROJECTION_VERSION,
            // A short prefix of the opaque host key: stable, comparable, and
            // not the provider's id for the request.
            attempt: binding.host_idempotency().key().short().to_string(),
            ordinal: binding.ordinal(),
            state: record.state,
            delivery: record.delivery_knowledge(),
            auto_retry_permitted: record.may_auto_retry(),
            origin: scope.origin(),
            family: scope.family(),
            dialect: binding.route().dialect(),
            model: binding.route().wire_model().to_string(),
            session: scope.session().short().to_string(),
            run: scope.run().map(|run| run.short().to_string()),
            created_at: record.created_at,
            state_changed_at: record.state_changed_at,
            settlement: record
                .settlement
                .as_ref()
                .map(|settlement| SettlementProjection {
                    outcome: settlement.outcome,
                    cancellation: settlement.cancellation,
                    audit: settlement.audit,
                    provider_receipt_present: settlement.receipt.provider_receipt.is_some(),
                    status_class: settlement
                        .receipt
                        .status
                        .map(|status| status_class(status).to_string()),
                    prompt_tokens: settlement.accounting.prompt_tokens,
                    completion_tokens: settlement.accounting.completion_tokens,
                    request_bytes: settlement.accounting.request_bytes,
                    response_bytes: settlement.accounting.response_bytes,
                    uncertainty: settlement.uncertainty,
                    settled_at: settlement.settled_at,
                }),
        }
    }

    /// One line an operator can read without it implying more than is known.
    pub fn summary(&self) -> String {
        let delivery = match self.delivery {
            DeliveryKnowledge::KnownDelivered => "reached the provider",
            DeliveryKnowledge::KnownNotDelivered => "did not reach the provider",
            DeliveryKnowledge::Unknown => "may or may not have reached the provider",
        };
        format!(
            "attempt {} (#{}) on {}: {delivery}",
            self.attempt, self.ordinal, self.model
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_send::identity::OpaqueId;
    use crate::provider_send::identity::{
        AttemptBinding, AttemptBindingSpec, RequestDigest, RouteIncarnation, SendScope,
    };
    use crate::provider_send::record::{
        AccountingRecord, HostIncarnationId, ReceiptRecord, Settlement,
    };
    use crate::provider_send::seams::{
        AuditGeneration, CapabilityGeneration, LifecycleGeneration, PrincipalGeneration,
        QueueOwnershipGeneration,
    };

    const WORKSPACE: &str = "/Users/someone/private-project";
    const SESSION: &str = "session-1234-private";
    const RUN: &str = "run-5678-private";
    const BASE_URL: &str = "https://internal.gateway.invalid/private/inference";
    const CREDENTIAL_BINDING: &str = "credential-binding-secret";
    const PROVIDER_RECEIPT: &str = "chatcmpl-private-provider-id";

    fn record(state: ProviderAttemptState, settlement: Option<Settlement>) -> ProviderAttempt {
        let spec = AttemptBindingSpec {
            scope: SendScope::new(
                WORKSPACE,
                SESSION,
                Some(RUN),
                SendOrigin::Orchestration,
                CallSiteFamily::GeneralPurposeSubagent,
            )
            .expect("scope"),
            principal: PrincipalGeneration::provisional(&["principal"]),
            capability: CapabilityGeneration::provisional(&["capability"]),
            lifecycle: LifecycleGeneration::provisional(&["lifecycle"]),
            queue: QueueOwnershipGeneration::provisional(&["queue"]),
            audit: AuditGeneration::provisional(&["audit"]),
            route: RouteIncarnation::new(
                BASE_URL,
                "operator-model-v2",
                WireDialect::OpenAiChatCompletions,
                "gateway_api_key",
                Some(CREDENTIAL_BINDING),
            ),
            request_digest: RequestDigest::of_body(b"a private prompt body"),
        };
        let mut record = ProviderAttempt::new(
            AttemptBinding::seal(spec, 3),
            HostIncarnationId::from_raw("host-1"),
            Utc::now(),
        );
        record.state = state;
        record.settlement = settlement;
        record
    }

    #[test]
    fn the_projection_redacts_every_private_input() {
        let settlement = Settlement {
            outcome: SettlementOutcome::Completed,
            cancellation: CancellationRecord::NotRequested,
            receipt: ReceiptRecord {
                provider_receipt: Some(crate::provider_send::identity::opaque_digest(
                    "t",
                    &[PROVIDER_RECEIPT],
                )),
                status: Some(200),
            },
            accounting: AccountingRecord {
                prompt_tokens: Some(11),
                completion_tokens: Some(22),
                request_bytes: 100,
                response_bytes: 200,
            },
            audit: AuditOutcome::Accounted,
            settled_at: Utc::now(),
            uncertainty: None,
        };
        let projection =
            ProviderAttemptProjection::of(&record(ProviderAttemptState::Settled, Some(settlement)));
        let json = serde_json::to_string(&projection).expect("serialize");
        for private in [
            WORKSPACE,
            SESSION,
            RUN,
            BASE_URL,
            "internal.gateway.invalid",
            CREDENTIAL_BINDING,
            PROVIDER_RECEIPT,
            "a private prompt body",
        ] {
            assert!(!json.contains(private), "projection leaked {private}");
        }
        // Exact status is coarsened; the operator's own model id stays visible.
        assert!(!json.contains("\"status\":200"));
        assert!(json.contains("2xx"));
        assert!(json.contains("operator-model-v2"));
        assert!(
            projection
                .settlement
                .expect("settlement")
                .provider_receipt_present
        );
    }

    #[test]
    fn unknown_delivery_is_shown_as_unknown_not_as_failure() {
        let settlement = Settlement {
            outcome: SettlementOutcome::Uncertain,
            cancellation: CancellationRecord::NotRequested,
            receipt: ReceiptRecord::default(),
            accounting: AccountingRecord {
                request_bytes: 100,
                ..AccountingRecord::default()
            },
            audit: AuditOutcome::Unresolved,
            settled_at: Utc::now(),
            uncertainty: Some(UncertaintyClass::ConnectionReset),
        };
        let projection = ProviderAttemptProjection::of(&record(
            ProviderAttemptState::Uncertain,
            Some(settlement),
        ));
        assert_eq!(projection.delivery, DeliveryKnowledge::Unknown);
        assert!(!projection.auto_retry_permitted);
        assert!(projection.summary().contains("may or may not"));
        assert_eq!(
            projection.settlement.expect("settlement").audit,
            AuditOutcome::Unresolved
        );
    }

    #[test]
    fn known_non_delivery_is_stated_plainly() {
        let projection =
            ProviderAttemptProjection::of(&record(ProviderAttemptState::NotSent, None));
        assert_eq!(projection.delivery, DeliveryKnowledge::KnownNotDelivered);
        assert!(projection.auto_retry_permitted);
        assert!(projection.summary().contains("did not reach"));
    }

    #[test]
    fn the_public_attempt_handle_is_opaque_and_stable() {
        let record = record(ProviderAttemptState::Sending, None);
        let first = ProviderAttemptProjection::of(&record);
        let second = ProviderAttemptProjection::of(&record);
        assert_eq!(first.attempt, second.attempt);
        assert_eq!(first.attempt.len(), 12);
        assert!(
            OpaqueId::parse(&first.attempt).is_err(),
            "short, not the key"
        );
    }
}
