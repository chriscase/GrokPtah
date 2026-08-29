//! Wire dialects and what each one may be told about idempotency (#478).
//!
//! The host always has an idempotency identity for an attempt. Whether that
//! identity is allowed onto the wire is a *per-dialect* question, and the answer
//! defaults to no. A dialect earns a header only by declaring one explicitly,
//! because sending an unrecognised `Idempotency-Key` to a gateway that has not
//! documented it is a guess dressed up as a guarantee.

use serde::{Deserialize, Serialize};

/// The provider dialects this host can physically speak.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireDialect {
    /// Grok Build proxy behind OIDC.
    GrokBuild,
    /// xAI chat completions with an API key.
    XaiChatCompletions,
    /// An OpenAI-compatible gateway configured by the operator.
    OpenAiChatCompletions,
}

impl WireDialect {
    pub const ALL: [Self; 3] = [
        Self::GrokBuild,
        Self::XaiChatCompletions,
        Self::OpenAiChatCompletions,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::GrokBuild => "grok_build",
            Self::XaiChatCompletions => "xai_chat_completions",
            Self::OpenAiChatCompletions => "openai_chat_completions",
        }
    }

    /// Whether this dialect has *explicitly* declared support for a request
    /// idempotency header, and under which name.
    ///
    /// All three current dialects answer `Unsupported`. That is the honest
    /// state of the world, not an oversight: none of them documents an
    /// idempotency header for `/chat/completions`. When one does, this is the
    /// single place that changes, and the wire test below starts requiring it.
    pub fn idempotency_support(self) -> DialectIdempotency {
        match self {
            Self::GrokBuild | Self::XaiChatCompletions | Self::OpenAiChatCompletions => {
                DialectIdempotency::Unsupported
            }
        }
    }

    /// Whether a provider-issued receipt identifier can be read from responses
    /// of this dialect. A receipt is the *provider's* identity for the request
    /// and is never conflated with the host idempotency key.
    pub fn receipt_source(self) -> ReceiptSource {
        match self {
            // All three dialects return an OpenAI-shaped `id` on the completion
            // object. It is opaque to us and is stored as opaque.
            Self::GrokBuild | Self::XaiChatCompletions | Self::OpenAiChatCompletions => {
                ReceiptSource::CompletionObjectId
            }
        }
    }
}

impl From<crate::gateway_config::ProviderDialect> for WireDialect {
    fn from(value: crate::gateway_config::ProviderDialect) -> Self {
        match value {
            crate::gateway_config::ProviderDialect::XaiChatCompletions => Self::XaiChatCompletions,
            crate::gateway_config::ProviderDialect::OpenAiChatCompletions => {
                Self::OpenAiChatCompletions
            }
        }
    }
}

/// Whether a dialect accepts a request idempotency header.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DialectIdempotency {
    /// The dialect has not declared support. Nothing is sent.
    Unsupported,
    /// The dialect documents this header name for request idempotency.
    Supported { header: &'static str },
}

impl DialectIdempotency {
    /// The header to add for this attempt, if any.
    pub fn header_for(
        self,
        host_identity: &super::identity::HostIdempotencyIdentity,
    ) -> Option<(&'static str, String)> {
        match self {
            Self::Unsupported => None,
            Self::Supported { header } => Some((header, host_identity.wire_value())),
        }
    }
}

/// Where a provider receipt comes from, if anywhere.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptSource {
    /// No receipt is available from this dialect.
    None,
    /// The `id` field of the completion object.
    CompletionObjectId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_send::identity::{fixtures, AttemptBinding};

    #[test]
    fn no_current_dialect_receives_an_idempotency_header() {
        let binding = AttemptBinding::seal(fixtures::spec("s", "b"), 1);
        for dialect in WireDialect::ALL {
            assert_eq!(
                dialect
                    .idempotency_support()
                    .header_for(binding.host_idempotency()),
                None,
                "{} must not be sent an undeclared idempotency header",
                dialect.as_str()
            );
        }
    }

    #[test]
    fn a_declaring_dialect_sends_the_opaque_host_value() {
        let binding = AttemptBinding::seal(fixtures::spec("s", "b"), 1);
        let support = DialectIdempotency::Supported {
            header: "Idempotency-Key",
        };
        let (name, value) = support
            .header_for(binding.host_idempotency())
            .expect("declared support sends a header");
        assert_eq!(name, "Idempotency-Key");
        assert!(value.starts_with("gp-1-"));
        assert!(!value.contains(' '));
    }

    #[test]
    fn gateway_dialects_map_onto_wire_dialects() {
        assert_eq!(
            WireDialect::from(crate::gateway_config::ProviderDialect::XaiChatCompletions),
            WireDialect::XaiChatCompletions
        );
        assert_eq!(
            WireDialect::from(crate::gateway_config::ProviderDialect::OpenAiChatCompletions),
            WireDialect::OpenAiChatCompletions
        );
    }
}
